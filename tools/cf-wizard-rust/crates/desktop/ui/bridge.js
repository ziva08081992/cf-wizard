/* Small, local IPC adapter. Contains no credentials or network client. */
(() => {
  "use strict";
  let nextId = 1;
  const pending = new Map();
  class Signal {
    constructor() { this.listeners = []; }
    connect(listener) { this.listeners.push(listener); }
    emit(...args) { for (const listener of this.listeners) listener(...args); }
  }
  const names = ["changed", "stubsChanged", "gitTaskProgress", "journalOpened", "notice", "indexFolderPrompt", "connectRequested"];
  const signals = Object.fromEntries(names.map(name => [name, new Signal()]));
  function call(method, args, callback) {
    const id = callback ? nextId++ : null;
    if (callback) pending.set(id, callback);
    window.ipc.postMessage(JSON.stringify({method, args, id}));
  }
  window.__cfNative = {
    reply(id, result) {
      const callback = pending.get(id);
      if (callback) { pending.delete(id); callback(result); }
    },
    signal(name, args) { signals[name]?.emit(...args); },
    call,
  };
  let connectionDone;
  let provider = "gitea";
  function gitDialog(callback) {
    const dialog = document.getElementById("cf-git-dialog");
    const form = document.getElementById("cf-git-form");
    call("gitInfo", [], settings => {
      for (const key of ["server", "repository", "directory", "branch", "cache", "interval"]) form.elements[key].value = settings[key] ?? "";
      provider = settings.provider || "gitea";
      for (const button of dialog.querySelectorAll("[data-provider]")) button.setAttribute("aria-pressed", String(button.dataset.provider === provider));
      form.elements.token.value = "";
      document.getElementById("git-remember").checked = false;
      document.getElementById("git-connection-status").textContent = "";
      connectionDone = callback;
      dialog.showModal();
      form.elements.server.focus();
    });
  }
  const bridge = {...signals};
  for (const method of ["snapshot", "groups", "rows", "markObject", "markStub", "editField", "pickPath", "openLink", "searchGitTask", "setGitBranch", "setSourceMode"]) {
    bridge[method] = (...args) => {
      const callback = typeof args.at(-1) === "function" ? args.pop() : undefined;
      call(method, args, callback);
    };
  }
  bridge.command = (name, callback) => {
    if (name === "git-connect") gitDialog(callback);
    else call("command", [name], callback);
  };
  window.qt = {webChannelTransport: {}};
  window.QWebChannel = class { constructor(_transport, callback) { callback({objects: {wizard: bridge}}); } };
  document.addEventListener("DOMContentLoaded", () => {
    signals.connectRequested.connect(() => gitDialog());
    const dialog = document.getElementById("cf-git-dialog");
    const form = document.getElementById("cf-git-form");
    const buttons = () => [...dialog.querySelectorAll("button, input")];
    function complete() { const callback = connectionDone; connectionDone = undefined; callback?.(); }
    dialog.addEventListener("close", () => { form.elements.token.value = ""; complete(); });
    document.getElementById("git-cancel").onclick = () => dialog.close();
    for (const button of dialog.querySelectorAll("[data-provider]")) button.onclick = () => {
      provider = button.dataset.provider;
      for (const item of dialog.querySelectorAll("[data-provider]")) item.setAttribute("aria-pressed", String(item === button));
      if (provider === "github") form.elements.server.value = "https://github.com";
      else if (["https://github.com", "https://api.github.com"].includes(form.elements.server.value)) form.elements.server.value = "";
      form.elements.token.value = "";
      document.getElementById("git-remember").checked = false;
    };
    document.getElementById("git-delete-token").onclick = () => call("deleteToken", [provider, form.elements.server.value.trim()], () => {});
    document.getElementById("git-pick-cache").onclick = () => call("pickCache", [form.elements.cache.value], path => { if (path) form.elements.cache.value = path; });
    form.addEventListener("submit", event => {
      event.preventDefault();
      if (!form.reportValidity()) return;
      const settings = Object.fromEntries(["server", "repository", "directory", "branch", "cache"].map(key => [key, form.elements[key].value.trim()]));
      settings.provider = provider;
      settings.interval = Number(form.elements.interval.value);
      const token = form.elements.token.value;
      const remember = document.getElementById("git-remember").checked;
      form.elements.token.value = "";
      buttons().forEach(button => button.disabled = true);
      document.getElementById("git-connection-status").textContent = "Подключаюсь…";
      call("connectGit", [settings, token, remember], state => {
        buttons().forEach(button => button.disabled = false);
        if (state?.activeGit && state?.statusKind !== "error") dialog.close();
        else document.getElementById("git-connection-status").textContent = state?.status || "Подключение не выполнено.";
      });
    });
    const prompt = document.getElementById("cf-index-prompt");
    signals.indexFolderPrompt.connect(folder => {
      document.getElementById("cf-index-prompt-text").textContent = "Открыт другой источник XML. Для него используется отдельный файл SQLite.\nТекущая папка индексов:\n" + folder + "\n\nВыбрать другую папку?";
      if (!prompt.open) prompt.showModal();
    });
    prompt.addEventListener("close", () => { if (prompt.returnValue === "choose") bridge.pickPath("cf-index"); });
    call("viewReady", []);
  });
})();
