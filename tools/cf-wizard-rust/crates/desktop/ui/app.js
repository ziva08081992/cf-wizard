/* Desktop presentation. Package decisions and native operations belong to Rust use cases. */
(() => {
  "use strict";
  const root = document.getElementById("cf-wizard-preview");
  const one = selector => root.querySelector(selector);
  const all = selector => root.querySelectorAll(selector);
  const action = name => one('[data-action="' + name + '"]');
  const fields = {
    export: one("#cf-source"),
    destination: one("#cf-result"),
    logs: one("#cf-logs"),
    platform_path: one("#cf-platform"),
    task: one("#cf-task"),
    index_directory: one("#cf-index"),
  };
  const buildMode = one("#cf-build-mode");
  const buildMenu = one("#cf-build-options");
  const gitBranch = one("#cf-git-branch");
  const gitBranchMenu = one("#cf-git-branch-options");
  const gitTaskKey = one("#cf-git-task-key");
  const gitTaskPeriod = one("#cf-git-task-period");
  const gitTaskPeriodMenu = one("#cf-git-task-period-options");
  const gitTaskDays = one("#cf-git-task-days");
  const groupIcons = {
    Subsystem: "layers", CommonModule: "code-xml", Document: "files",
    Catalog: "book-open", DataProcessor: "settings-2", InformationRegister: "table-2",
    BusinessProcess: "workflow",
  };
  const search = one("#cf-search");
  const selectedOnly = one("#cf-selected-only");
  const groups = one("[data-groups]");
  const notice = one('dialog[aria-labelledby="cf-dialog-title"]');
  const journal = one(".cf-journal");
  const help = one(".cf-help");
  const preferences = one("#cf-preferences");
  const sourceButtons = all("[data-source-mode]");
  let bridge, state, queryTimer, focusedKey;
  let generation = 0;
  let selected = new Map();
  const pendingSelection = new Map();
  const expanded = new Set();

  function icon(name) {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    for (const [key, value] of Object.entries({
      viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", "stroke-width": "2",
      "stroke-linecap": "round", "stroke-linejoin": "round", "aria-hidden": "true",
      "data-lucide": name,
    })) svg.setAttribute(key, value);
    svg.innerHTML = cfIcons[name];
    return svg;
  }

  function element(tag, text, className) {
    const node = document.createElement(tag);
    if (text) node.textContent = text;
    if (className) node.className = className;
    return node;
  }

  function checkbox(label) {
    const wrapper = element("label", "", "cf-checkbox");
    const check = element("input");
    check.type = "checkbox";
    check.setAttribute("aria-label", label);
    const indicator = element("span", "", "cf-check-indicator");
    indicator.setAttribute("aria-hidden", "true");
    indicator.append(element("span", "", "cf-check-tick"));
    wrapper.append(check, indicator);
    return {wrapper, check};
  }

  function updateCheck(check, checked, disabled) {
    if (check.checked !== checked) check.checked = checked;
    check.disabled = disabled;
    // Paint the mark in the same render pass as the selection summary.
    check.nextElementSibling.classList.toggle("cf-checked", checked);
  }

  function setTab(name) {
    for (const tab of all("[data-tab]")) {
      const active = tab.dataset.tab === name;
      tab.setAttribute("aria-selected", active);
      one("#" + tab.getAttribute("aria-controls")).hidden = !active;
    }
  }

  function renderWhole() {
    const rows = [...selected.values()].map(object => {
      const row = element("tr");
      row.append(element("td", object.name), element("td", object.type));
      return row;
    });
    one("[data-whole-rows]").replaceChildren(...rows);
    one("[data-empty-whole]").hidden = !!rows.length;
    one("[data-whole-count]").textContent = rows.length;
  }

  function renderSelection() {
    selected = new Map(state.selected.map(object => [object.key, object]));
    for (const {object, checked} of pendingSelection.values()) {
      if (checked) selected.set(object.key, object);
      else selected.delete(object.key);
    }
    const pending = pendingSelection.size > 0;
    action("preview").disabled = pending || !state.canPreview;
    action("build").disabled = pending || !state.canBuild;
    action("package").disabled = pending || !state.canOpenPackage;
    one("[data-selected-count]").textContent = "Отмечено " + selected.size;
    renderWhole();
    updateChecks();
  }

  function updateChecks() {
    for (const check of all("[data-object-check]")) {
      updateCheck(check, selected.has(check.dataset.objectCheck), state.busy);
    }
    const excluded = new Set(state.excluded);
    for (const check of all("[data-stub-toggle]")) {
      const included = !excluded.has(check.dataset.stubToggle);
      const disabled = check.dataset.required === "true" || !state.canMarkStubs || pendingSelection.size > 0;
      updateCheck(check, included, disabled);
      check.closest("tr").classList.toggle("cf-stub-omitted", !included);
    }
    const count = state.stubTotal - excluded.size;
    one("[data-stub-count]").textContent = excluded.size ? count + "/" + state.stubTotal : count;
  }

  function renderStubs(stubs) {
    const table = one("[data-stub-table]");
    for (const body of [...table.tBodies]) body.remove();
    for (const group of stubs) {
      const body = table.createTBody();
      body.classList.add("cf-stub-branch", "cf-stub-collapsed");
      const heading = body.insertRow();
      heading.className = "cf-stub-group";
      const label = element("th");
      label.scope = "rowgroup";
      label.colSpan = 3;
      const toggle = element("button", "", "cf-stub-group-toggle");
      toggle.type = "button";
      toggle.setAttribute("aria-expanded", "false");
      const arrow = element("span", "▸", "cf-stub-arrow");
      arrow.setAttribute("aria-hidden", "true");
      toggle.append(arrow, icon(groupIcons[group.kind] || "list"), document.createTextNode(group.label),
                    element("span", String(group.rows.length), "cf-group-count"));
      toggle.addEventListener("click", () => {
        const collapsed = body.classList.toggle("cf-stub-collapsed");
        toggle.setAttribute("aria-expanded", String(!collapsed));
      });
      label.append(toggle);
      heading.append(label);
      for (const stub of group.rows) {
        const row = body.insertRow();
        const name = row.insertCell();
        name.append(element("span", stub.name), element("span", stub.type, "cf-type"));
        const reason = row.insertCell();
        reason.textContent = stub.reason;
        reason.title = stub.owners;
        const choice = row.insertCell();
        choice.className = "cf-stub-choice";
        const {wrapper, check} = checkbox("Включить заглушку " + stub.name);
        check.dataset.stubToggle = stub.key;
        check.dataset.required = stub.required;
        check.title = stub.required ? "Обязательная ссылка" : "Ссылка найдена только в комментариях";
        check.addEventListener("change", () => {
          updateCheck(check, check.checked, check.disabled);
          bridge.markStub(stub.key, check.checked);
        });
        choice.append(wrapper);
        if (stub.required) choice.append(element("span", "обяз.", "cf-stub-required"));
      }
    }
    one("[data-empty-stubs]").hidden = !!stubs.length;
    if (stubs.length) setTab("stubs");
    if (state) updateChecks();
  }

  function objectRow(object) {
    const row = element("div", "", "cf-object");
    const name = element("button", object.name, "cf-object-name");
    name.type = "button";
    name.title = object.type + "." + object.name;
    const {wrapper, check} = checkbox("Отметить " + object.name);
    check.dataset.objectCheck = object.key;
    updateCheck(check, selected.has(object.key), state.busy);
    row.classList.toggle("cf-focused", object.key === focusedKey);
    const focus = () => {
      one(".cf-object.cf-focused")?.classList.remove("cf-focused");
      focusedKey = object.key;
      row.classList.add("cf-focused");
    };
    name.addEventListener("click", focus);
    name.addEventListener("keydown", event => {
      if (event.key !== " " || event.repeat) return;
      event.preventDefault();
      if (!state.busy) check.click();
    });
    check.addEventListener("change", () => {
      focus();
      const intent = {object, checked: check.checked};
      pendingSelection.set(object.key, intent);
      renderSelection();
      bridge.markObject(object.key, intent.checked, confirmed => {
        if (pendingSelection.get(object.key) === intent) pendingSelection.delete(object.key);
        renderState(confirmed);
      });
    });
    row.append(name, wrapper);
    return row;
  }

  function groupSection(group, query, autoOpen, onlySelected) {
    const details = element("details");
    details.dataset.kind = group.kind;
    const summary = element("summary");
    summary.append(icon(groupIcons[group.kind] || "list"), document.createTextNode(group.label),
                   element("span", String(group.count), "cf-group-count"));
    const rows = element("div");
    const more = element("button", "Показать ещё", "cf-button cf-more");
    more.type = "button";
    more.hidden = true;
    details.append(summary, rows, more);
    let offset = 0;
    let loading = false;
    function load() {
      if (loading || offset >= group.count) return;
      loading = true;
      more.disabled = true;
      bridge.rows(group.kind, query, offset, onlySelected, objects => {
        loading = false;
        if (!details.isConnected) return;
        rows.append(...objects.map(objectRow));
        offset += objects.length;
        more.hidden = offset >= group.count || !objects.length;
        more.disabled = false;
      });
    }
    more.addEventListener("click", load);
    details.addEventListener("toggle", () => {
      if (!query && !onlySelected && !state.gitTaskResult) {
        if (details.open) expanded.add(group.kind);
        else expanded.delete(group.kind);
      }
      if (details.open && offset === 0) load();
    });
    details.open = autoOpen || !query && expanded.has(group.kind);
    return details;
  }

  function renderGroups() {
    const query = search.value.trim();
    const onlySelected = selectedOnly.checked;
    const request = ++generation;
    bridge.groups(query, onlySelected, items => {
      if (request !== generation) return;
      const found = items.reduce((sum, group) => sum + group.count, 0);
      one("[data-selected-count]").title = "Найдено " + found;
      const autoOpen = found <= 800 && (onlySelected || !!query || !!state.gitTaskResult);
      groups.replaceChildren(...items.map(group => groupSection(group, query, autoOpen, onlySelected)));
      const empty = one("[data-empty-tree]");
      empty.hidden = !!items.length;
      empty.textContent = !state.configuration ? "Откройте XML-выгрузку."
        : onlySelected && !state.selected.length ? "Объекты ещё не отмечены." : "По этому запросу ничего не найдено.";
    });
  }

  function setChoiceMenu(trigger, menu, open) {
    menu.hidden = !open;
    trigger.setAttribute("aria-expanded", String(open));
    if (open) (menu.querySelector('[aria-checked="true"]') || menu.firstElementChild)?.focus();
  }

  function renderChoice(menu, label, value) {
    let caption = value;
    for (const option of menu.children) {
      const checked = option.value === value;
      option.setAttribute("aria-checked", String(checked));
      if (checked) caption = option.textContent;
    }
    label.textContent = caption;
  }

  function bindChoiceMenu(trigger, menu, choose) {
    const setOpen = open => setChoiceMenu(trigger, menu, open);
    trigger.addEventListener("click", () => setOpen(menu.hidden));
    trigger.addEventListener("keydown", event => {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        if (menu.hidden) setOpen(true);
      }
    });
    document.addEventListener("pointerdown", event => {
      if (!menu.hidden && !trigger.contains(event.target) && !menu.contains(event.target)) setOpen(false);
    });
    menu.addEventListener("pointerover", event => {
      event.target.closest("button")?.focus({preventScroll: true});
    });
    menu.addEventListener("keydown", event => {
      const options = [...menu.children];
      const count = options.length;
      const focused = options.indexOf(document.activeElement);
      const next = {
        ArrowDown: (focused + 1) % count,
        ArrowUp: (focused + count - 1) % count,
        Home: 0,
        End: count - 1,
      }[event.key];
      if (next !== undefined && options[next]) {
        event.preventDefault();
        options[next].focus();
      } else if (event.key === "Escape" || event.key === "Tab") {
        if (event.key === "Escape") event.preventDefault();
        setOpen(false);
        trigger.focus();
      }
    });
    menu.addEventListener("click", event => {
      const option = event.target.closest("button");
      if (!option) return;
      setOpen(false);
      trigger.focus();
      choose(option.value);
    });
  }

  function gitCommentQueryError(value) {
    if (/[\u0000-\u001f\u007f-\u009f]/u.test(value)) {
      return "Поисковый текст не должен содержать управляющие символы.";
    }
    const query = value.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");
    if (new TextEncoder().encode(query).length > 256) {
      return "Слишком длинный поисковый текст: максимум 256 байт UTF-8.";
    }
    return query ? "" : "Укажите ключ задачи или фрагмент комментария.";
  }

  function updateGitTaskButton() {
    const custom = gitTaskPeriod.value === "custom";
    const valid = !custom || (Number.isSafeInteger(gitTaskDays.valueAsNumber) && gitTaskDays.valueAsNumber > 0);
    one("[data-git-task-days]").hidden = !custom;
    gitTaskDays.disabled = !custom || !state?.gitTaskReady || state.busy;
    gitTaskDays.setAttribute("aria-invalid", String(!valid));
    one("#cf-git-task-days-error").hidden = valid;
    const queryError = gitCommentQueryError(gitTaskKey.value);
    const showQueryError = Boolean(gitTaskKey.value && queryError);
    gitTaskKey.setAttribute("aria-invalid", String(showQueryError));
    one("#cf-git-comment-error").textContent = queryError;
    one("#cf-git-comment-error").hidden = !showQueryError;
    action("search-git-task").disabled = !state?.gitTaskReady || state.busy || Boolean(queryError) || !valid;
  }

  function renderGitTask(started) {
    const scope = state.gitTaskScope;
    const result = state.gitTaskResult;
    one("[data-git-task]").hidden = state.sourceMode !== "git";
    one("[data-git-task-hint]").hidden = state.gitTaskReady;
    gitTaskKey.disabled = gitTaskPeriod.disabled = state.busy || !state.gitTaskReady;
    if ((gitTaskPeriod.disabled || state.sourceMode !== "git") && !gitTaskPeriodMenu.hidden) {
      setChoiceMenu(gitTaskPeriod, gitTaskPeriodMenu, false);
    }
    updateGitTaskButton();
    action("cancel-git-task").hidden = !scope;
    action("cancel-git-task").disabled = state.gitTaskCancelling;
    one("[data-git-task-progress]").hidden = !scope;
    one("[data-git-task-scope]").textContent = scope;
    if (started) one("[data-git-task-scanned]").textContent = "Читаю историю коммитов…";
    one("[data-git-task-result]").hidden = !result;
    if (result) {
      one("[data-git-task-caption]").textContent = result.scope;
      const counts = ["Коммитов " + result.commits, "Файлов " + result.files, "Объектов " + result.objects];
      if (result.unmapped) counts.push("Путей без объекта " + result.unmapped);
      one("[data-git-task-counts]").textContent = counts.join(" · ");
      action("mark-git-task").disabled = state.busy || !result.canMark;
      action("clear-git-task").disabled = state.busy;
    }
  }

  function findGitTask() {
    if (!action("search-git-task").disabled) {
      const days = gitTaskPeriod.value === "custom" ? String(gitTaskDays.valueAsNumber) : "90";
      bridge.searchGitTask(gitTaskKey.value, gitTaskPeriod.value === "all", days);
    }
  }

  function renderState(next) {
    const catalogueChanged = !state || state.catalogRevision !== next.catalogRevision;
    const taskFilterChanged = !state || state.gitTaskRevision !== next.gitTaskRevision
      || Boolean(state.gitTaskResult) !== Boolean(next.gitTaskResult);
    const gitTaskStarted = next.gitTaskScope && state?.gitTaskScope !== next.gitTaskScope;
    const selectionChanged = !state || state.selected.length !== next.selected.length
      || next.selected.some((object, index) => object.key !== state.selected[index].key);
    if (!state || state.values.build_mode !== next.values.build_mode) {
      renderChoice(buildMenu, one("#cf-build-value"), next.values.build_mode);
      one("#cf-platforms").replaceChildren(...next.platforms.map(path => {
        const option = element("option");
        option.value = path;
        return option;
      }));
    }
    const branchesChanged = !state || state.gitBranches.length !== next.gitBranches.length
      || next.gitBranches.some((branch, index) => branch !== state.gitBranches[index]);
    if (branchesChanged) {
      setChoiceMenu(gitBranch, gitBranchMenu, false);
      gitBranchMenu.replaceChildren(...next.gitBranches.map(name => {
        const option = element("button", name);
        option.type = "button";
        option.value = name;
        option.tabIndex = -1;
        option.setAttribute("role", "menuitemradio");
        return option;
      }));
    }
    if (branchesChanged || state.gitBranch !== next.gitBranch) {
      renderChoice(gitBranchMenu, one("#cf-git-branch-value"), next.gitBranch);
      gitBranch.title = next.gitBranch;
    }
    state = next;
    const dependencyErrors = state.dependencyErrors || [];
    one("[data-dependency-errors]").hidden = dependencyErrors.length === 0;
    const errorLabels = {missing_object:"Не найден объект", missing_member:"Не найден элемент", missing_candidate:"Нет допустимого кандидата", missing_file:"Не найден обязательный файл"};
    one("[data-dependency-error-items]").replaceChildren(...dependencyErrors.map(error =>
      element("li", `${errorLabels[error.kind] || "Ошибка зависимости"}: ${error.target}. Требуется для ${error.owner}. Источник: ${error.source}`)));
    renderGitTask(gitTaskStarted);
    if (catalogueChanged) pendingSelection.clear();
    for (const [key, input] of Object.entries(fields)) {
      if (document.activeElement !== input) input.value = state.values[key];
      if (key !== "task") input.title = state.values[key];
      input.disabled = state.busy;
    }
    buildMode.disabled = state.busy;
    if (state.busy && !buildMenu.hidden) setChoiceMenu(buildMode, buildMenu, false);
    for (const button of all("[data-picker]")) button.disabled = state.busy;
    const automatic = state.values.build_mode === "auto";
    fields.platform_path.readOnly = automatic;
    fields.platform_path.placeholder = state.platformPlaceholder;
    fields.platform_path.title = state.values.platform_path || state.platformPlaceholder;
    const platformPicker = one("[data-picker=cf-platform]");
    platformPicker.disabled = state.busy || automatic;
    platformPicker.title = automatic ? "Выберите способ сборки для ручного указания пути" : state.platformPlaceholder;
    const gitMode = state.sourceMode === "git";
    for (const button of sourceButtons) {
      button.setAttribute("aria-pressed", String(button.dataset.sourceMode === state.sourceMode));
      button.disabled = state.busy;
    }
    one("#cf-git-source").hidden = !gitMode;
    gitBranch.disabled = state.busy;
    if ((state.busy || !gitMode) && !gitBranchMenu.hidden) setChoiceMenu(gitBranch, gitBranchMenu, false);
    for (const name of ["open-source", "settings", "git-connect"]) action(name).disabled = state.busy;
    one("[data-open-source]").textContent = gitMode ? state.gitOpenText : "Открыть выгрузку";
    action("open-source").title = gitMode ? state.gitOpenText : state.values.export || "Выбрать папку XML-выгрузки";
    one("#cf-git-location").textContent = state.gitLocation;
    const poll = one("[data-git-poll]");
    poll.textContent = state.gitPollInfo;
    poll.hidden = !gitMode || !state.gitPollInfo;
    action("logs").disabled = state.busy;
    action("refresh-index").disabled = !state.canRefreshIndex;
    one("[data-index-command]").textContent = state.indexExists ? "Обновить индекс" : "Создать индекс";
    one("#cf-index-info").textContent = state.indexInfo;
    action("journal").disabled = !state.canOpenJournal;
    search.disabled = state.busy;
    selectedOnly.disabled = state.busy;
    one("[data-source-summary]").hidden = !state.configuration;
    one("[data-config-name]").textContent = state.configuration;
    one("[data-active-source]").textContent = state.activeSource;
    const indexStatus = one("[data-index-status]");
    indexStatus.title = state.indexInfo;
    indexStatus.classList.toggle("cf-index-needed", !state.indexReady);
    one("[data-index-label]").textContent = state.indexReady ? "Индекс готов"
      : state.indexExists ? "Нужно обновить индекс" : "Индекс ещё не создан";
    one("[data-platform-version]").textContent = state.platformLabel;
    one("[data-status]").textContent = state.status;
    const dot = one(".cf-status-main .cf-status-dot");
    dot.classList.toggle("cf-waiting", ["idle", "busy"].includes(state.statusKind));
    dot.classList.toggle("cf-error", state.statusKind === "error");
    renderSelection();
    if (catalogueChanged) {
      expanded.clear();
      focusedKey = undefined;
    }
    if (catalogueChanged || taskFilterChanged || selectedOnly.checked && selectionChanged) renderGroups();
  }

  function showNotice(title, text) {
    one("#cf-dialog-title").textContent = title;
    one("[data-dialog-text]").textContent = text;
    one("[data-dialog-links]").hidden = title !== "О программе";
    if (!notice.open) notice.showModal();
  }

  function showPreferences() {
    if (!preferences.open) preferences.showModal();
  }

  function connectGit() {
    preferences.close();
    bridge.command("git-connect", () => {
      const reopen = () => {
        showPreferences();
        action("git-connect").focus();
      };
      if (notice.open) notice.addEventListener("close", reopen, {once: true});
      else reopen();
    });
  }

  function showJournal(path, text) {
    one("[data-journal-path]").textContent = path;
    const content = one("[data-journal-text]");
    content.value = text;
    if (!journal.open) journal.showModal();
    content.setSelectionRange(0, 0);
    content.scrollTop = 0;
    content.scrollLeft = 0;
  }

  for (const placeholder of all("i[data-lucide]")) placeholder.replaceWith(icon(placeholder.dataset.lucide));
  const showHelp = () => { if (!help.open) help.showModal(); };
  action("help").addEventListener("click", showHelp);
  document.addEventListener("keydown", event => {
    if (event.key === "F1") {
      event.preventDefault();
      showHelp();
    }
  });
  for (const tab of all("[data-tab]")) tab.addEventListener("click", () => setTab(tab.dataset.tab));
  if (typeof QWebChannel === "undefined" || typeof qt === "undefined") {
    one("[data-status]").textContent = "Не удалось подключить окно к мастеру.";
    return;
  }

  new QWebChannel(qt.webChannelTransport, channel => {
    bridge = channel.objects.wizard;
    bridge.changed.connect(renderState);
    bridge.gitTaskProgress.connect(count => {
      if (state?.gitTaskScope && !state.gitTaskCancelling) {
        one("[data-git-task-scanned]").textContent = "Просмотрено коммитов " + count;
      }
    });
    for (const input of [gitTaskKey, gitTaskDays]) {
      input.addEventListener("input", () => {
        updateGitTaskButton();
        if (input === gitTaskKey && !input.value.trim() && state?.gitTaskResult && !state.busy) {
          bridge.command("clear-git-task");
        }
      });
      input.addEventListener("keydown", event => {
        if (event.key === "Enter") {
          event.preventDefault();
          findGitTask();
        }
      });
    }
    bridge.stubsChanged.connect(renderStubs);
    bridge.journalOpened.connect(showJournal);
    bridge.notice.connect(showNotice);
    for (const [key, input] of Object.entries(fields)) {
      input.addEventListener(["logs", "index_directory"].includes(key) ? "change" : "input", () => bridge.editField(key, input.value));
    }
    bindChoiceMenu(buildMode, buildMenu, value => bridge.editField("build_mode", value));
    bindChoiceMenu(gitBranch, gitBranchMenu, value => bridge.setGitBranch(value));
    bindChoiceMenu(gitTaskPeriod, gitTaskPeriodMenu, value => {
      gitTaskPeriod.value = value;
      renderChoice(gitTaskPeriodMenu, one("#cf-git-task-period-value"), value);
      updateGitTaskButton();
    });
    for (const button of sourceButtons) {
      button.addEventListener("click", () => bridge.setSourceMode(button.dataset.sourceMode));
    }
    fields.export.addEventListener("keydown", event => {
      if (event.key === "Enter" && state?.sourceMode === "local") {
        event.preventDefault();
        bridge.command("open-source");
      }
    });
    for (const button of all("[data-action]")) {
      const name = button.dataset.action;
      if (name === "help") continue;
      const handler = name === "settings" ? showPreferences
        : name === "git-connect" ? connectGit
        : name === "search-git-task" ? findGitTask : () => bridge.command(name);
      button.addEventListener("click", handler);
    }
    for (const button of all("[data-picker]")) {
      button.addEventListener("click", () => bridge.pickPath(button.dataset.file || button.dataset.picker));
    }
    for (const link of all("[data-link]")) {
      link.addEventListener("click", event => {
        event.preventDefault();
        bridge.openLink(link.dataset.link);
      });
    }
    search.addEventListener("input", () => {
      clearTimeout(queryTimer);
      queryTimer = setTimeout(renderGroups, 90);
    });
    selectedOnly.addEventListener("change", renderGroups);
    bridge.snapshot(initial => {
      renderState(initial);
      if (initial.sourceMode === "local" && initial.values.export.trim() && !initial.configuration && !initial.busy) {
        bridge.command("open-source");
      }
    });
  });
})();
