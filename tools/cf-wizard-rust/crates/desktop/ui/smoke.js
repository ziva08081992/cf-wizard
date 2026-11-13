/* Invoked only by the explicit --exercise-ui validation harness. */
(async () => {
  const checks = [];
  const rpc = (method, ...args) => new Promise(resolve => window.__cfNative.call(method, args, resolve));
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  const check = (name, value) => { if (!value) throw new Error(name); checks.push(name); };
  async function wait(predicate, name) {
    const start = performance.now();
    while (!predicate()) { if (performance.now() - start > 15000) throw new Error(name); await sleep(40); }
  }
  try {
    let state = await rpc("snapshot");
    check("source auto-open", !!state.configuration && !state.busy);
    const group = document.querySelector('details[data-kind="CommonModule"]');
    check("Russian metadata tree", !!group && group.querySelector("summary").textContent.includes("Общие модули"));
    group.open = true;
    await wait(() => group.querySelector("[data-object-check]"), "object rows loaded");
    let row = group.querySelector(".cf-object");
    row.querySelector(".cf-object-name").click();
    check("row click does not select", (await rpc("snapshot")).selected.length === 0);
    let checkbox = row.querySelector("[data-object-check]");
    const key = checkbox.dataset.objectCheck;
    checkbox.click();
    await wait(() => checkbox.nextElementSibling.classList.contains("cf-checked"), "immediate check paint");
    state = await rpc("snapshot");
    check("native selection confirmed", state.selected.some(object => object.key === key));
    checkbox.click();
    await wait(() => !checkbox.nextElementSibling.classList.contains("cf-checked"), "immediate uncheck paint");
    check("native uncheck confirmed", (await rpc("snapshot")).selected.length === 0);
    checkbox.click();
    await rpc("snapshot");
    const selectedOnly = document.querySelector("#cf-selected-only");
    selectedOnly.click();
    await wait(() => document.querySelectorAll("[data-object-check]").length === 1, "selected-only filter");
    check("selected-only filter", true);
    selectedOnly.click();
    state = await rpc("command", "preview");
    check("preview completes", state.statusKind === "ok" && state.canOpenJournal && !state.busy);
    await rpc("command", "journal");
    await wait(() => document.querySelector(".cf-journal").open, "built-in journal");
    check("journal rendered in app", document.querySelector("[data-journal-text]").value.includes("dependencies-ready"));
    document.querySelector(".cf-journal button[type=submit]").click();
    document.querySelector('[data-action="help"]').click();
    check("embedded help", document.querySelector(".cf-help").open);
    document.querySelector(".cf-help button[type=submit]").click();
    await rpc("editField", "task", "08/11/2026: произвольный комментарий");
    check("arbitrary task comment", (await rpc("snapshot")).values.task.includes("произвольный"));
    await rpc("setSourceMode", "git");
    await rpc("command", "open-source");
    await wait(() => document.querySelector("#cf-git-dialog").open, "first Git open shows connection dialog");
    document.querySelector('[data-provider="github"]').click();
    const apiButton = document.querySelector('[data-provider="github"]');
    check("GitHub strategy choice", apiButton.getAttribute("aria-pressed") === "true");
    check("provider text contrast", getComputedStyle(apiButton).color !== getComputedStyle(apiButton).backgroundColor);
    check("provider server default", document.querySelector("#git-server").value === "https://github.com");
    document.querySelector("#git-cancel").click();
    await rpc("setSourceMode", "local");
    window.__cfNative.call("smokeResult", [{loaded: true, exercised: true, checks, pythonRuntime: false, qtLoaded: false}]);
  } catch (error) {
    window.__cfNative.call("smokeResult", [{loaded: false, exercised: true, checks, error: String(error)}]);
  }
})();
