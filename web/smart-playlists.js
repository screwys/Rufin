import { api, state } from "./connection.js";
import { $, button, el, icon, run, setIcon, tr } from "./ui.js";

// The editor's choices from core, and the playlist it edits: its key, or none for a new one.
let choices = null;
// The definition being edited, in core's saved form.
let draft = null;
let folder = null;
let countTimer = null;
let countRequest = null;
// The Playlist values the In playlist picker lists, read once per editor.
let playlists = null;
// The entry or picker the suggestion list shows under: {anchor}.
let suggesting = null;

// The library the page shows, which values and counts read in.
const scope = () => ({
  ...(state.source ? { source: state.source } : {}),
  ...(state.selectedLibrary ? { folder: state.selectedLibrary } : {}),
});

const fieldOf = (name) => choices.fields.find((field) => field.field === name);
const operatorOf = (field, name) => field.operators.find((operator) => operator.operator === name);
const copy = (value) => (value == null ? null : structuredClone(value));

// Opens New Smart Playlist, in the playlist folder `folder` when one is given, or Edit Smart
// Playlist for the playlist `key`.
async function openSmartPlaylist({ key = null, folder: into = null } = {}) {
  choices = await api(`/smart-playlists/editor${key == null ? "" : `?key=${key}`}`);
  draft = copy(choices.definition);
  folder = choices.key == null ? into : null;
  playlists = null;
  const editing = choices.key != null;
  $("smart-title").textContent = editing ? tr("Edit Smart Playlist") : tr("New Smart Playlist");
  $("smart-save").textContent = editing ? tr("Save") : tr("Create");
  $("smart-name").value = choices.name;
  $("smart-count").textContent = "";
  $("smart-templates").hidden = editing;
  $("smart-template-list").replaceChildren(
    ...choices.templates.map((template) => {
      const chip = el("button", "detail-genre-chip", template.name);
      chip.type = "button";
      chip.addEventListener("click", () => applyTemplate(template));
      return chip;
    }),
  );
  $("smart-sort").replaceChildren(...choices.sorts.map((sort) => new Option(sort.title, sort.sort)));
  $("smart-period").replaceChildren(...choices.periods.map((period) => new Option(period.title, period.period)));
  const sources = choices.sources.map((source) => new Option(source.name, source.id));
  if (draft.source_id && !choices.sources.some((source) => source.id === draft.source_id))
    sources.push(new Option(draft.source_id, draft.source_id));
  $("smart-source").replaceChildren(new Option(tr("All"), ""), ...sources);
  $("smart-source-row").hidden = choices.sources.length < 2 && !draft.source_id;
  showOrder();
  changed(true);
  $("smart-dialog").showModal();
  $("smart-name").focus();
}

// A template's rules and order; its name only while the name is empty.
function applyTemplate(template) {
  const definition = copy(template.definition);
  for (const name of ["matching", "conditions", "sort_field", "descending", "activity_period"])
    draft[name] = definition[name];
  draft.limit = definition.limit ?? null;
  if (!$("smart-name").value.trim()) $("smart-name").value = template.name;
  showOrder();
  changed(true);
}

function showOrder() {
  $("smart-sort").value = draft.sort_field;
  $("smart-descending").setAttribute("aria-pressed", String(draft.descending));
  setIcon($("smart-descending").firstElementChild, draft.descending ? "rufin-sort-name-descending-symbolic" : "rufin-sort-name-symbolic");
  $("smart-descending").disabled = draft.sort_field === "Random";
  $("smart-limit").value = draft.limit ?? "";
  $("smart-period").value = draft.activity_period;
  $("smart-source").value = draft.source_id ?? "";
}

// Whether the rules or the order read plays, so Plays from applies.
function readsPlays() {
  const rules = draft.conditions.flatMap((condition) => condition.Rule ? [condition.Rule] : condition.Group.rules);
  return choices.sorts.find((sort) => sort.sort === draft.sort_field).reads_plays
    || rules.some((rule) => fieldOf(rule.field).reads_plays);
}

// After an edit: the rules drawn again when their rows change, and the count read again.
function changed(rows = false) {
  if (rows) showRules();
  $("smart-period-row").hidden = !readsPlays();
  validate();
  clearTimeout(countTimer);
  countTimer = setTimeout(() => run(async () => {
    countRequest?.abort();
    countRequest = new AbortController();
    const counted = await api("/smart-playlists/count", "POST", { definition: draft, ...scope() }, countRequest.signal);
    $("smart-count").textContent = counted.text;
  }), 300);
}

// A limit is blank, for none, or a positive whole number.
function limitValue() {
  const text = $("smart-limit").value.trim();
  if (!text) return null;
  return /^\d+$/.test(text) && Number(text) > 0 ? Number(text) : undefined;
}

function validate() {
  const limit = limitValue();
  $("smart-limit").setAttribute("aria-invalid", String(limit === undefined));
  $("smart-save").disabled = !$("smart-name").value.trim() || limit === undefined;
}

// The list's match reads "Match all of these rules", a group's "All of these", as on GTK.
function matchSelect(value, set, titles) {
  const select = el("select", "smart-matching");
  select.setAttribute("aria-label", tr("Match"));
  select.append(...choices.matches.map((match, index) => new Option(titles[index], match.matching)));
  select.value = value;
  select.addEventListener("change", () => {
    set(select.value);
    changed();
  });
  return select;
}

function labeled(label, name, action, className = "smart-add") {
  const node = button(label, action, name, className);
  node.append(document.createTextNode(label));
  return node;
}

function showRules() {
  hideSuggestions();
  $("smart-match").replaceChildren(
    el("span", "", tr("Match")),
    matchSelect(draft.matching, (value) => (draft.matching = value), choices.matches.map((match) => match.title)),
    el("span", "", tr("of these rules")),
  );
  const host = $("smart-rules");
  host.replaceChildren();
  draft.conditions.forEach((condition, index) => {
    if (condition.Rule) {
      host.append(ruleRow(condition.Rule, (rule) => {
        if (rule) draft.conditions[index] = { Rule: rule };
        else draft.conditions.splice(index, 1);
      }));
      return;
    }
    const group = condition.Group;
    const box = el("div", "smart-group");
    const head = el("div", "smart-group-head");
    head.append(
      matchSelect(group.matching, (value) => (group.matching = value), [tr("All"), tr("Any")]),
      el("span", "", tr("of these")),
    );
    head.append(button(tr("Remove group"), () => {
      draft.conditions.splice(index, 1);
      changed(true);
    }, "rufin-list-remove-symbolic"));
    const rules = el("div", "smart-rules");
    group.rules.forEach((rule, at) => rules.append(ruleRow(rule, (next) => {
      if (next) group.rules[at] = next;
      else group.rules.splice(at, 1);
    })));
    const add = labeled(tr("Add rule"), "rufin-list-add-symbolic", () => {
      group.rules.push(copy(choices.fields[0].rule));
      changed(true);
    });
    box.append(head, rules, add);
    host.append(box);
  });
  host.hidden = !host.childElementCount;
}

// A rule's field, operator, value and remove; `replace` takes the changed rule, or none to
// remove it.
function ruleRow(rule, replace) {
  const row = el("div", "smart-rule");
  const field = fieldOf(rule.field);
  const fields = el("select", "smart-field");
  fields.setAttribute("aria-label", tr("Field"));
  let section = null;
  for (const choice of choices.fields) {
    if (choice.section !== section?.label) {
      section = el("optgroup");
      section.label = choice.section;
      fields.append(section);
    }
    section.append(new Option(choice.title, choice.field));
  }
  fields.value = rule.field;
  fields.addEventListener("change", () => {
    replace(copy(fieldOf(fields.value).rule));
    changed(true);
  });
  const operators = el("select", "smart-operator");
  operators.setAttribute("aria-label", tr("Condition"));
  operators.append(...field.operators.map((operator) => new Option(operator.title, operator.operator)));
  operators.value = rule.operator;
  operators.addEventListener("change", () => {
    const next = operatorOf(field, operators.value);
    const kept = operatorOf(field, rule.operator).kind === next.kind;
    replace({ ...rule, operator: next.operator, value: kept ? rule.value : copy(next.default) });
    changed(true);
  });
  const value = el("div", "smart-value");
  value.append(...valueControls(rule, field, operatorOf(field, rule.operator)));
  row.append(fields, operators, value, button(tr("Remove rule"), () => {
    replace(null);
    changed(true);
  }, "rufin-list-remove-symbolic"));
  return row;
}

// The value's entries for its kind. Edits change the rule in place; the row stays.
function valueControls(rule, field, operator) {
  const set = (value) => {
    rule.value = value;
    changed();
  };
  const number = (stored, label, store) => {
    const scale = field.scale;
    const input = el("input");
    input.type = "number";
    input.min = String(operator.min / scale);
    input.max = String(operator.max / scale);
    input.step = String(1 / scale);
    input.value = String(stored / scale);
    input.setAttribute("aria-label", label);
    input.addEventListener("input", () => {
      if (!Number.isFinite(input.valueAsNumber)) return;
      stored = Math.min(operator.max, Math.max(operator.min, Math.round(input.valueAsNumber * scale)));
      store(stored);
    });
    input.addEventListener("change", () => (input.value = String(stored / scale)));
    return input;
  };
  const text = (initial, placeholder, label, store) => {
    const input = el("input");
    input.value = initial;
    input.placeholder = placeholder;
    input.setAttribute("aria-label", label);
    input.addEventListener("input", () => store(input.value));
    return input;
  };
  switch (operator.kind) {
    case "Text": {
      const input = text(rule.value.Text, field.placeholder, field.title, (entered) => set({ Text: entered }));
      if (field.suggests) bindSuggestions(input, field.field, (choice) => {
        input.value = choice.value;
        set({ Text: choice.value });
      });
      return [input];
    }
    case "Number":
    case "Days": {
      const input = number(rule.value.Number, field.title, (count) => set({ Number: count }));
      return operator.kind === "Days" ? [input, el("span", "smart-unit", tr("days"))] : [input];
    }
    case "NumberRange": {
      const range = rule.value.NumberRange;
      return [
        number(range.min, tr("Minimum"), (min) => set({ NumberRange: { ...rule.value.NumberRange, min } })),
        el("span", "smart-unit", tr("to")),
        number(range.max, tr("Maximum"), (max) => set({ NumberRange: { ...rule.value.NumberRange, max } })),
      ];
    }
    case "Date":
      return [text(rule.value.Date, "YYYY-MM-DD", field.title, (date) => set({ Date: date }))];
    case "DateRange": {
      const range = rule.value.DateRange;
      return [
        text(range.start, "YYYY-MM-DD", tr("Minimum"), (start) => set({ DateRange: { ...rule.value.DateRange, start } })),
        el("span", "smart-unit", tr("to")),
        text(range.end, "YYYY-MM-DD", tr("Maximum"), (end) => set({ DateRange: { ...rule.value.DateRange, end } })),
      ];
    }
    case "Bool": {
      const select = el("select");
      select.setAttribute("aria-label", field.title);
      select.append(new Option(tr("Yes"), "true"), new Option(tr("No"), "false"));
      select.value = String(rule.value.Bool);
      select.addEventListener("change", () => set({ Bool: select.value === "true" }));
      return [select];
    }
    case "Playlist":
      return [playlistPicker(rule, field, set)];
    default:
      return [];
  }
}

// In playlist: one of the playlists core lists, shown by name.
function playlistPicker(rule, field, set) {
  const picker = el("button", "smart-picker");
  picker.type = "button";
  picker.setAttribute("aria-label", field.title);
  picker.setAttribute("aria-haspopup", "listbox");
  const name = el("span");
  picker.append(name, icon("rufin-pan-down-symbolic"));
  const params = new URLSearchParams({ field: field.field, ...scope(), ...(draft.source_id ? { from: draft.source_id } : {}) });
  playlists ??= api(`/smart-playlists/values?${params}`).then((result) => result.values);
  // A saved playlist no longer listed shows its id, as on GTK.
  const show = (values) => (name.textContent = values.find((choice) => choice.value === rule.value.Playlist)?.name ?? rule.value.Playlist);
  run(async () => show(await playlists));
  picker.addEventListener("click", () => run(async () => {
    if (suggesting?.anchor === picker) return hideSuggestions();
    const values = await playlists;
    showSuggestions(picker, values, rule.value.Playlist, (choice) => {
      set({ Playlist: choice.value });
      show(values);
    });
  }));
  return picker;
}

// Values of `field` under an entry as its text changes; typed text stays the value.
function bindSuggestions(input, field, pick) {
  let timer = null;
  let request = null;
  const read = () => {
    clearTimeout(timer);
    timer = setTimeout(() => run(async () => {
      request?.abort();
      request = new AbortController();
      const params = new URLSearchParams({ field, q: input.value, ...scope(), ...(draft.source_id ? { from: draft.source_id } : {}) });
      const { values } = await api(`/smart-playlists/values?${params}`, "GET", undefined, request.signal);
      if (document.activeElement === input) showSuggestions(input, values, null, pick);
    }), 150);
  };
  input.setAttribute("autocomplete", "off");
  input.setAttribute("aria-autocomplete", "list");
  input.addEventListener("input", read);
  input.addEventListener("focus", read);
  input.addEventListener("blur", () => {
    clearTimeout(timer);
    request?.abort();
    if (suggesting?.anchor === input) hideSuggestions();
  });
  input.addEventListener("keydown", (event) => {
    if (suggesting?.anchor !== input) return;
    const items = [...$("smart-suggestions").children];
    const at = items.findIndex((item) => item.classList.contains("active"));
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const next = event.key === "ArrowDown" ? (at + 1) % items.length : at <= 0 ? items.length - 1 : at - 1;
      items.forEach((item, index) => item.classList.toggle("active", index === next));
      items[next]?.scrollIntoView({ block: "nearest" });
    } else if (event.key === "Enter" && at >= 0) {
      event.preventDefault();
      items[at].click();
    } else if (event.key === "Escape") {
      event.preventDefault();
      hideSuggestions();
    }
  });
}

function showSuggestions(anchor, values, current, pick) {
  const list = $("smart-suggestions");
  if (!values.length) return hideSuggestions();
  suggesting = { anchor };
  list.replaceChildren(...values.map((choice) => {
    const item = el("button", "menu-item");
    item.type = "button";
    item.setAttribute("role", "option");
    item.setAttribute("aria-selected", String(choice.value === current));
    item.classList.toggle("active", choice.value === current);
    if (anchor.classList.contains("smart-picker")) item.append(icon("rufin-playlists-symbolic"));
    item.append(el("span", "smart-suggestion-name", choice.name), el("small", "", String(choice.tracks)));
    // The entry keeps focus while a value is picked.
    item.addEventListener("pointerdown", (event) => event.preventDefault());
    item.addEventListener("click", () => {
      hideSuggestions();
      pick(choice);
    });
    return item;
  }));
  if (!list.matches(":popover-open")) list.showPopover();
  const rect = anchor.getBoundingClientRect();
  list.style.minWidth = `${rect.width}px`;
  list.style.left = `${Math.max(8, Math.min(rect.left, innerWidth - list.offsetWidth - 8))}px`;
  list.style.top = `${rect.bottom + list.offsetHeight + 12 <= innerHeight ? rect.bottom + 4 : Math.max(8, rect.top - list.offsetHeight - 4)}px`;
}

function hideSuggestions() {
  suggesting = null;
  if ($("smart-suggestions").matches(":popover-open")) $("smart-suggestions").hidePopover();
}

function init() {
  $("smart-name").addEventListener("input", validate);
  $("smart-add-rule").addEventListener("click", () => {
    draft.conditions.push({ Rule: copy(choices.fields[0].rule) });
    changed(true);
  });
  $("smart-add-group").addEventListener("click", () => {
    draft.conditions.push({ Group: { matching: "All", rules: [copy(choices.fields[0].rule)] } });
    changed(true);
  });
  $("smart-sort").addEventListener("change", () => {
    draft.sort_field = $("smart-sort").value;
    showOrder();
    changed();
  });
  $("smart-descending").addEventListener("click", () => {
    draft.descending = !draft.descending;
    showOrder();
    changed();
  });
  $("smart-limit").addEventListener("input", () => {
    const limit = limitValue();
    if (limit !== undefined) draft.limit = limit;
    changed();
  });
  $("smart-period").addEventListener("change", () => {
    draft.activity_period = $("smart-period").value;
    changed();
  });
  $("smart-source").addEventListener("change", () => {
    draft.source_id = $("smart-source").value || null;
    playlists = null;
    changed(true);
  });
  $("smart-dialog").addEventListener("close", () => {
    clearTimeout(countTimer);
    countRequest?.abort();
    hideSuggestions();
  });
  $("smart-dialog").addEventListener("scroll", (event) => {
    if (event.target !== $("smart-suggestions")) hideSuggestions();
  }, true);
  document.addEventListener("pointerdown", (event) => {
    if (suggesting && !event.target.closest("#smart-suggestions") && !suggesting.anchor.contains(event.target)) hideSuggestions();
  });
  $("smart-dialog").addEventListener("keydown", (event) => {
    if (event.key === "Escape" && suggesting) {
      event.preventDefault();
      hideSuggestions();
    }
  });
  $("smart-form").addEventListener("submit", (event) => {
    event.preventDefault();
    run(async () => {
      if ($("smart-save").disabled) return;
      $("smart-save").disabled = true;
      try {
        await api("/smart-playlists", "POST", {
          key: choices.key,
          name: $("smart-name").value.trim(),
          definition: { ...draft, limit: limitValue() },
          folder,
        });
        $("smart-dialog").close();
      } finally {
        validate();
      }
    });
  });
}

export { openSmartPlaylist, init };
