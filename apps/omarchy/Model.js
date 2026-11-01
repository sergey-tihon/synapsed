function normalizeEndpoint(text) {
  var match = /^http:\/\/(127\.0\.0\.1|localhost|\[::1\]):([0-9]{1,5})\/?$/.exec(String(text));
  if (!match) return null;
  var port = Number(match[2]);
  if (!Number.isInteger(port) || port < 1 || port > 65535) return null;
  return "http://" + match[1] + ":" + match[2];
}

function webUrl(endpoint, deviceId, characteristicId) {
  if (!deviceId || !characteristicId) {
    return endpoint + "/";
  }
  return (
    endpoint + "/#/devices/" + encodeURIComponent(deviceId) + "?characteristic=" + encodeURIComponent(characteristicId)
  );
}

function parseSnapshot(line) {
  try {
    var snapshot = JSON.parse(line);
    if (
      !snapshot ||
      snapshot.apiVersion !== 1 ||
      !snapshot.node ||
      typeof snapshot.node !== "object" ||
      Array.isArray(snapshot.node) ||
      !Array.isArray(snapshot.drivers) ||
      !Array.isArray(snapshot.devices)
    ) {
      return null;
    }
    return snapshot;
  } catch (_) {
    return null;
  }
}

function supportsStep(ch) {
  if (!ch || !ch.access || !ch.access.write || !ch.spec) return false;
  return ch.spec.kind === "enum" || (ch.spec.kind === "number" && ch.spec.step !== null);
}

function controlFor(ch) {
  if (!ch.access.write) return "readonly";
  var spec = ch.spec;
  if (spec.kind === "action") return "invoke";
  if (spec.kind === "boolean") return "toggle";
  if (spec.kind === "enum") return spec.options.length <= 4 ? "choice" : "stepper";
  if (
    spec.kind === "number" &&
    Number.isFinite(spec.minimum) &&
    Number.isFinite(spec.maximum) &&
    spec.minimum < spec.maximum
  )
    return "slider";
  if (spec.kind === "number" && spec.step !== null) return "stepper";
  if (spec.kind === "number" || spec.kind === "string") return "web";
  return "readonly";
}

function setArgument(spec, value) {
  var result = Number(value);
  if (Number.isFinite(spec.minimum)) result = Math.max(spec.minimum, result);
  if (Number.isFinite(spec.maximum)) result = Math.min(spec.maximum, result);
  if (spec.integer) result = Math.round(result);
  return String(result);
}

function stepArgument(ticks) {
  return Math.max(-100, Math.min(100, Math.trunc(Number(ticks) || 0)));
}

function formatValue(ch) {
  var value;
  if (ch.spec.kind === "action") value = "Action";
  else if (ch.value === null) value = "Not yet observed";
  else if (ch.spec.kind === "boolean") value = ch.value ? "On" : "Off";
  else if (ch.spec.kind === "enum") {
    var option = ch.spec.options.find(function (item) {
      return item.value === ch.value;
    });
    value = option ? option.label : String(ch.value);
  } else if (ch.spec.kind === "number") {
    value = String(ch.value) + (ch.spec.unit ? " " + ch.spec.unit : "");
  } else if (ch.value === "") value = "Empty string";
  else value = String(ch.value);
  if (ch.status !== "ok") value += " · " + (ch.status === "error" ? "error" : "unavailable");
  return value;
}

function problemDrivers(snapshot) {
  if (!snapshot || !Array.isArray(snapshot.drivers)) return [];
  return snapshot.drivers
    .filter(function (driver) {
      return ["ambiguous", "permission_denied", "error"].includes(driver.state);
    })
    .map(function (driver) {
      return (
        driver.id + ": " + driver.state + (driver.error && driver.error.message ? " — " + driver.error.message : "")
      );
    });
}

function tooltip(connection, snapshot, problems) {
  var text;
  if (connection === "online") {
    var count = snapshot && Array.isArray(snapshot.devices) ? snapshot.devices.length : 0;
    text = "Synapsed — Online · " + count + " device" + (count === 1 ? "" : "s");
  } else if (connection === "connecting") text = "Synapsed — Connecting";
  else if (connection === "offline") text = "Synapsed — Offline (stale)";
  else if (connection === "failed") text = "Synapsed — synapsectl unavailable";
  else text = "Synapsed — Invalid endpoint setting";
  if (problems && problems.length)
    text += " · " + problems.length + " driver error" + (problems.length === 1 ? "" : "s");
  return text;
}

function commandArgs(endpoint, op, deviceId, characteristicId, argument) {
  var args = ["synapsectl", "--endpoint", endpoint, "--json", op, deviceId, characteristicId];
  if (op === "set" || op === "step") args.push(String(argument));
  return args;
}

function watchArgs(endpoint) {
  return ["synapsectl", "--endpoint", endpoint, "--json", "watch"];
}

function characteristicSections(device) {
  var groups = device.groups || [];
  var characteristics = device.characteristics || [];
  if (groups.length === 0) {
    return [{ label: null, characteristics: characteristics.slice() }];
  }

  var sections = [];
  var matched = new Set();
  groups.forEach(function (group) {
    var members = characteristics.filter(function (characteristic) {
      if (characteristic.group !== group.id) return false;
      matched.add(characteristic);
      return true;
    });
    if (members.length > 0) sections.push({ label: group.label, characteristics: members });
  });
  var unmatched = characteristics.filter(function (characteristic) {
    return !matched.has(characteristic);
  });
  if (unmatched.length > 0) sections.push({ label: null, characteristics: unmatched });
  return sections;
}
