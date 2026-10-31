/* Public metadata is supplied by the plugin. This inspector never contacts the daemon. */
(() => {
  "use strict";
  const client = SDPIComponents.streamDeckClient;
  const selectors = {
    deviceId: document.getElementById("device"),
    characteristicId: document.getElementById("characteristic"),
    pressDeviceId: document.getElementById("press-device"),
    pressCharacteristicId: document.getElementById("press-characteristic"),
  };
  const connection = document.getElementById("connection");
  const editors = {
    key: { root: document.getElementById("key-editor"), metadata: null, signature: "", pending: null },
    press: { root: document.getElementById("press-editor"), metadata: null, signature: "", pending: null },
  };
  let settings = {};
  let selection = { deviceId: "", characteristicId: "", pressDeviceId: "", pressCharacteristicId: "" };
  let initialized = false;
  let applying = false;
  let alive = true;
  let requestId = 0;
  let settingsVersion = 0;
  const ownWrites = [];
  let queuedSettings = null;

  function settingsSignature(value) {
    return JSON.stringify(value, (_key, item) =>
      item && typeof item === "object" && !Array.isArray(item)
        ? Object.fromEntries(
            Object.keys(item)
              .sort()
              .map((key) => [key, item[key]]),
          )
        : item,
    );
  }

  function synchronizeSelectors() {
    applying = true;
    try {
      for (const [field, element] of Object.entries(selectors)) element.value = selection[field];
    } finally {
      applying = false;
    }
  }

  function selectionFromSettings() {
    return {
      deviceId: settings.binding?.deviceId ?? "",
      characteristicId: settings.binding?.characteristicId ?? "",
      pressDeviceId: settings.pressBinding?.deviceId ?? "",
      pressCharacteristicId: settings.pressBinding?.characteristicId ?? "",
    };
  }

  function requestCatalog() {
    if (!initialized || !alive) return;
    void client.send("sendToPlugin", { event: "getCatalog", selection }).catch(() => {
      if (alive) connection.textContent = "Cannot communicate with the Synapsed plugin.";
    });
  }

  function acceptSettings(value) {
    if (!alive || !value || typeof value !== "object" || Array.isArray(value)) return;
    settingsVersion += 1;
    const signature = settingsSignature(value);
    const ownWrite = ownWrites.findIndex((write) => write.signature === signature);
    if (ownWrite >= 0) {
      // An acknowledgement confirms a send, not the newest local settings merge base.
      ownWrites.splice(ownWrite, 1);
    } else {
      settings = value;
      selection = selectionFromSettings();
      // Genuine external settings supersede any local save not yet sent.
      queuedSettings = null;
    }
    initialized = true;
    synchronizeSelectors();
    requestCatalog();
    if (ownWrite >= 0) void flushSettings();
  }

  async function persist(changes, removed = []) {
    const next = { ...settings, ...changes };
    for (const field of removed) delete next[field];
    if (settingsSignature(next) === settingsSignature(settings)) return;
    settings = next;
    queuedSettings = next;
    await flushSettings();
  }

  async function flushSettings() {
    if (!alive || !queuedSettings || ownWrites.length >= 32) return;
    const next = queuedSettings;
    queuedSettings = null;
    const write = { signature: settingsSignature(next) };
    // Never evict an unacknowledged write: even an arbitrarily late old ack must be recognized.
    // At capacity, coalesce unsent edits into the newest merged settings instead.
    ownWrites.push(write);
    try {
      await client.setSettings(next);
    } catch {
      const index = ownWrites.indexOf(write);
      if (index >= 0) ownWrites.splice(index, 1);
      if (alive) connection.textContent = "Could not save Stream Deck settings.";
      void flushSettings();
    }
  }

  function bindingFor(slot) {
    const deviceId = slot === "key" ? selection.deviceId : selection.pressDeviceId;
    const characteristicId = slot === "key" ? selection.characteristicId : selection.pressCharacteristicId;
    return deviceId && characteristicId ? { deviceId, characteristicId } : null;
  }

  async function selectionChanged(field) {
    if (!initialized || applying || !alive) return;
    const value = String(selectors[field].value ?? "");
    if (value === selection[field]) return;
    selection[field] = value;
    const slot = field.startsWith("press") ? "press" : "key";
    if (field === "deviceId") selection.characteristicId = "";
    if (field === "pressDeviceId") selection.pressCharacteristicId = "";
    synchronizeSelectors();
    const editor = editors[slot];
    editor.pending = null;
    editor.signature = "";
    editor.metadata = null;
    renderEditor(slot, null);
    const binding = bindingFor(slot);
    const bindingField = slot === "key" ? "binding" : "pressBinding";
    const commandField = slot === "key" ? "keyCommand" : "pressCommand";
    await persist(binding ? { [bindingField]: binding } : {}, binding ? [commandField] : [bindingField, commandField]);
    if (alive) requestCatalog();
  }

  function appendOption(select, value, label, disabled = false) {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = label;
    option.disabled = disabled;
    select.append(option);
  }

  function metadataText(characteristic) {
    const spec = characteristic.spec;
    let text = `${characteristic.label}: ${spec.kind}; ${characteristic.access.write ? "writable" : "read-only"}`;
    if (spec.kind === "number") {
      text += `; ${spec.integer ? "integer" : "finite number"}`;
      if (spec.minimum !== null) text += `; minimum ${spec.minimum}`;
      if (spec.maximum !== null) text += `; maximum ${spec.maximum}`;
      if (spec.step !== null) text += `; step ${spec.step}`;
      if (spec.unit) text += ` ${spec.unit}`;
    } else if (spec.kind === "string") text += `; at most ${spec.maxLength} Unicode characters`;
    if (characteristic.status !== "ok") text += `; ${characteristic.status} (last value stale)`;
    if (characteristic.error) text += `; ${characteristic.error.code}: ${characteristic.error.message}`;
    return text;
  }

  function commandLabel(command) {
    if (!command) return "Display only";
    if (command.op === "step") return `Step ${command.steps > 0 ? "+" : ""}${command.steps}`;
    return command.op === "toggle" ? "Toggle" : "Invoke";
  }

  function renderEditor(slot, characteristic) {
    const editor = editors[slot];
    editor.metadata = characteristic;
    const commandField = slot === "key" ? "keyCommand" : "pressCommand";
    const saved = settings[commandField];
    const signature = JSON.stringify([
      bindingFor(slot),
      characteristic?.spec,
      characteristic?.access,
      characteristic?.operations,
      saved,
    ]);
    if (editor.signature === signature) {
      if (editor.description && characteristic) editor.description.textContent = metadataText(characteristic);
      return;
    }
    editor.signature = signature;
    editor.pending = null;
    editor.root.replaceChildren();
    editor.description = null;
    if (!characteristic) {
      const message = document.createElement("span");
      message.textContent = bindingFor(slot)
        ? "Missing characteristic. The saved binding is retained."
        : slot === "press"
          ? "No secondary binding: dial press uses the primary key behavior."
          : "Select a device and characteristic. Unbound actions show Configure.";
      editor.root.append(message);
      return;
    }
    const description = document.createElement("span");
    description.textContent = metadataText(characteristic);
    editor.description = description;
    editor.root.append(description);
    const operation = document.createElement("select");
    operation.setAttribute("aria-label", `${slot} operation`);
    appendOption(operation, "default", `Default — ${commandLabel(characteristic.defaultCommand)}`);
    const labels = { set: "Set value", step: "Step (signed count)", toggle: "Toggle", invoke: "Invoke" };
    for (const op of characteristic.operations) appendOption(operation, op, labels[op]);
    if (saved && !characteristic.operations.includes(saved.op))
      appendOption(operation, saved.op, `Unsupported saved operation: ${saved.op}`, true);
    operation.value = saved?.op ?? "default";
    editor.root.append(operation);
    const valueContainer = document.createElement("div");
    editor.root.append(valueContainer);
    const error = document.createElement("span");
    error.className = "error";
    error.setAttribute("role", "alert");
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = "Apply command";
    editor.root.append(button, error);
    let valueInput = null;

    function updateValueEditor() {
      valueContainer.replaceChildren();
      valueInput = null;
      error.textContent = "";
      const op = operation.value;
      const spec = characteristic.spec;
      if (op === "set" && (spec.kind === "boolean" || spec.kind === "enum")) {
        valueInput = document.createElement("select");
        let placeholder = "";
        if (spec.kind === "boolean") {
          appendOption(valueInput, "", "Choose true or false", true);
          appendOption(valueInput, "true", "On (true)");
          appendOption(valueInput, "false", "Off (false)");
        } else {
          placeholder = "__synapsed_choose__";
          while (spec.options.some((option) => option.value === placeholder)) placeholder += "_";
          appendOption(valueInput, placeholder, "Choose an enum value", true);
          for (const option of spec.options) appendOption(valueInput, option.value, option.label);
        }
        const savedValue = saved?.op === "set" ? saved.value : undefined;
        valueInput.value = savedValue === undefined ? placeholder : String(savedValue);
        if (
          savedValue !== undefined &&
          !Array.from(valueInput.options).some((option) => option.value === String(savedValue))
        ) {
          appendOption(valueInput, String(savedValue), "Unsupported saved value", true);
          valueInput.value = String(savedValue);
        }
      } else if (op === "set" || op === "step") {
        valueInput = document.createElement("input");
        valueInput.type = "text";
        if (op === "step") {
          valueInput.inputMode = "numeric";
          valueInput.placeholder = "-100 to 100; zero is a no-op";
          valueInput.value = saved?.op === "step" ? String(saved.steps) : "";
        } else {
          valueInput.value = saved?.op === "set" ? String(saved.value) : "";
          if (spec.kind === "number") {
            valueInput.inputMode = spec.integer ? "numeric" : "decimal";
            valueInput.placeholder = "Complete finite number";
          } else valueInput.placeholder = "Literal string (empty is allowed)";
        }
      }
      if (valueInput) {
        valueInput.setAttribute("aria-label", `${slot} ${op === "step" ? "step count" : "value"}`);
        valueContainer.append(valueInput);
      }
      button.disabled = operation.value !== "default" && !characteristic.operations.includes(operation.value);
    }
    operation.addEventListener("change", updateValueEditor);
    updateValueEditor();
    button.addEventListener("click", async () => {
      if (!alive || editor.pending || button.disabled) return;
      const binding = bindingFor(slot);
      if (!binding) return;
      const id = ++requestId;
      editor.pending = { id, binding, commandField, error, button, operation, valueInput };
      button.disabled = true;
      operation.disabled = true;
      if (valueInput) valueInput.disabled = true;
      error.textContent = "";
      try {
        await client.send("sendToPlugin", {
          event: "validateCommand",
          slot,
          requestId: id,
          binding,
          op: operation.value,
          text: valueInput?.value ?? "",
        });
      } catch {
        if (!alive || editor.pending?.id !== id) return;
        finishValidation(editor, "Cannot communicate with the plugin.");
      }
    });
  }

  function finishValidation(editor, message) {
    const pending = editor.pending;
    if (!pending) return;
    editor.pending = null;
    pending.button.disabled = false;
    pending.operation.disabled = false;
    if (pending.valueInput) pending.valueInput.disabled = false;
    pending.error.textContent = message;
  }

  function receivedMessage(event) {
    if (!alive || !initialized) return;
    const payload = event.payload;
    if (!payload || typeof payload !== "object") return;
    if (payload.event === "catalog") {
      // Ignore catalog work issued for a superseded PI selection.
      if (settingsSignature(payload.selection) !== settingsSignature(selection)) return;
      connection.textContent =
        payload.configurationError ??
        {
          connected: "Synapsed connected",
          connecting: "Synapsed connecting — values are stale",
          disconnected: "Synapsed offline — values are stale; commands disabled",
          incompatible: "Synapsed incompatible — commands disabled",
        }[payload.connection] ??
        "Synapsed unavailable";
      renderEditor("key", payload.primary);
      renderEditor("press", payload.press);
    } else if (payload.event === "commandValidation") {
      const editor = editors[payload.slot];
      const pending = editor?.pending;
      if (
        !pending ||
        pending.id !== payload.requestId ||
        settingsSignature(pending.binding) !== settingsSignature(bindingFor(payload.slot))
      )
        return;
      finishValidation(editor, payload.error ?? "Command saved.");
      if (payload.error) return;
      void persist(
        payload.command ? { [pending.commandField]: payload.command } : {},
        payload.command ? [] : [pending.commandField],
      ).then(() => {
        if (alive) {
          requestCatalog();
        }
      });
    }
  }

  function receivedSettings(event) {
    acceptSettings(event.payload?.settings);
  }
  client.didReceiveSettings.subscribe(receivedSettings);
  client.sendToPropertyInspector.subscribe(receivedMessage);
  for (const [field, element] of Object.entries(selectors)) {
    element.addEventListener("valuechange", () => {
      void selectionChanged(field);
    });
  }
  window.addEventListener(
    "pagehide",
    () => {
      alive = false;
      client.didReceiveSettings.unsubscribe(receivedSettings);
      client.sendToPropertyInspector.unsubscribe(receivedMessage);
      for (const editor of Object.values(editors)) editor.pending = null;
      ownWrites.length = 0;
      queuedSettings = null;
    },
    { once: true },
  );
  const version = settingsVersion;
  void client
    .getConnectionInfo()
    .then((info) => {
      if (alive && settingsVersion === version) acceptSettings(info.actionInfo?.payload?.settings);
    })
    .catch(() => {
      if (alive) connection.textContent = "Cannot load Stream Deck settings.";
    });
  void client
    .getSettings()
    .then((payload) => {
      if (alive && settingsVersion === version) acceptSettings(payload.settings);
    })
    .catch(() => {
      if (alive) connection.textContent = "Cannot load Stream Deck settings.";
    });
})();
