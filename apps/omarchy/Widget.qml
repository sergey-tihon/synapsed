import QtQuick
import QtQuick.Controls
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model

Panel {
  id: root

  moduleName: "sergey-tihon.synapsed"
  ipcTarget: "sergey-tihon.synapsed"
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  readonly property string endpoint: Model.normalizeEndpoint(String(setting("endpoint", "http://127.0.0.1:24816"))) || ""
  property string connection: endpoint === "" ? "invalid" : "connecting"
  property var snapshot: null
  property string lastError: ""
  property var pending: ({})
  property var rowErrors: ({})
  property var wheelAccumulators: ({})

  function rowKey(deviceId, characteristicId) {
    return deviceId + "\n" + characteristicId
  }

  function canCommand(device, key) {
    return connection === "online" && device.status === "online" && !pending[key]
  }

  function openWeb(deviceId, characteristicId) {
    if (!endpoint) return;
    Quickshell.execDetached(["omarchy-launch-browser", Model.webUrl(endpoint, deviceId, characteristicId)]);
  }

  function dispatch(deviceId, characteristicId, op, argument) {
    var key = rowKey(deviceId, characteristicId);
    if (!snapshot) return;
    var device = snapshot.devices.find(function(candidate) { return candidate.id === deviceId; });
    if (!device || !canCommand(device, key)) return;
    var nextPending = Object.assign({}, pending);
    nextPending[key] = true;
    pending = nextPending;
    var nextErrors = Object.assign({}, rowErrors);
    delete nextErrors[key];
    rowErrors = nextErrors;
    var args = Model.commandArgs(endpoint, op, deviceId, characteristicId, argument);
    var process = commandProcess.createObject(root, { command: args, rowKey: key });
    if (!process) {
      finishCommand(key, 1, "Could not start synapsectl.");
      return;
    }
    process.running = true;
  }

  function finishCommand(key, code, stderr) {
    var nextPending = Object.assign({}, pending);
    delete nextPending[key];
    pending = nextPending;
    var nextErrors = Object.assign({}, rowErrors);
    if (code !== 0) {
      var firstLine = String(stderr || "").split(/\r?\n/)[0];
      nextErrors[key] = firstLine || "Command failed (exit " + code + ").";
    } else {
      delete nextErrors[key];
    }
    rowErrors = nextErrors;
  }

  function handleWheel(device, characteristic, key, delta) {
    if (!Model.supportsStep(characteristic) || !canCommand(device, key)) return;
    var wheel = Util.wheelSteps(wheelAccumulators[key] || 0, delta);
    var nextAccumulators = Object.assign({}, wheelAccumulators);
    nextAccumulators[key] = wheel.remainder;
    wheelAccumulators = nextAccumulators;
    var steps = Model.stepArgument(wheel.steps);
    if (steps !== 0) dispatch(device.id, characteristic.id, "step", steps);
  }

  onEndpointChanged: {
    snapshot = null;
    lastError = "";
    connection = endpoint === "" ? "invalid" : "connecting";
    watchProc.running = false;
    if (endpoint !== "") restartWatch.start();
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    iconComponent: Component {
      BrandGlyph {
        anchors.fill: parent
        foregroundColor: button.active && button.useActiveColor ? button.activeColor : button.foreground
      }
    }
    dimmed: root.connection !== "online"
    active: Model.problemDrivers(root.snapshot).length > 0
    tooltipText: Model.tooltip(root.connection, root.snapshot, Model.problemDrivers(root.snapshot))
    onPressed: function(mouseButton) {
      if (mouseButton === Qt.RightButton) root.openWeb(null, null);
      else root.toggle();
    }
  }

  Process {
    id: watchProc
    command: root.endpoint ? Model.watchArgs(root.endpoint) : []
    running: root.endpoint !== ""

    stdout: SplitParser {
      onRead: function(line) {
        var received = Model.parseSnapshot(line);
        if (received) {
          root.snapshot = received;
          root.connection = "online";
          root.lastError = "";
        } else {
          root.lastError = "Unexpected synapsectl watch output.";
        }
      }
    }
    stderr: SplitParser {
      onRead: function(line) {
        root.lastError = line;
        if (line.indexOf("synapsectl: disconnected") === 0) {
          root.connection = "offline";
        } else if (line.indexOf("synapsectl: connecting") === 0 && !root.snapshot) {
          root.connection = "connecting";
        }
      }
    }
    onExited: function(code) {
      root.connection = root.snapshot ? "offline" : "failed";
      if (!root.lastError) root.lastError = "synapsectl is not installed in PATH or failed to start.";
      if (root.endpoint !== "") restartWatch.start();
    }
  }

  Timer {
    id: restartWatch
    interval: 5000
    repeat: false
    onTriggered: if (root.endpoint !== "") watchProc.running = true
  }

  Component {
    id: commandProcess
    Process {
      property string rowKey: ""
      stderr: StdioCollector { id: commandStderr }
      onExited: function(code) {
        root.finishCommand(rowKey, code, commandStderr.text);
        destroy();
      }
    }
  }

  PopupCard {
    id: popup
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    contentWidth: popup.fittedContentWidth(Style.space(380))
    contentHeight: popup.fittedContentHeight(column.implicitHeight, Style.space(560))

    Flickable {
      anchors.fill: parent
      contentHeight: column.implicitHeight
      clip: true
      interactive: contentHeight > height
      boundsBehavior: Flickable.StopAtBounds
      ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

      Column {
        id: column
        width: parent.width
        spacing: Style.space(10)

        Text {
          text: "Synapsed"
          color: Commons.Color.popups.text
          font.family: Style.font.family
          font.pixelSize: Style.font.title
          font.bold: true
        }
        Text {
          width: parent.width
          text: root.connection === "online" ? "Online · " + (root.snapshot && root.snapshot.node ? root.snapshot.node.name : "Synapsed")
            : root.connection === "connecting" ? "Connecting…"
            : root.connection === "offline" ? "Offline — showing last observed values (stale)"
            : root.connection === "failed" ? "synapsectl unavailable — " + root.lastError
            : "Invalid endpoint setting"
          color: Commons.Color.popups.text
          wrapMode: Text.Wrap
        }
        Repeater {
          model: Model.problemDrivers(root.snapshot)
          Text {
            required property string modelData
            width: parent.width
            text: modelData
            color: Commons.Color.urgent
            wrapMode: Text.Wrap
          }
        }
        Text {
          visible: !root.snapshot
          text: "No snapshot received."
          color: Commons.Color.popups.text
        }
        Text {
          visible: !!root.snapshot && root.snapshot.devices.length === 0
          text: "No devices discovered."
          color: Commons.Color.popups.text
        }

        Repeater {
          model: root.snapshot ? root.snapshot.devices : []
          delegate: Column {
            id: deviceSection
            required property var modelData
            width: column.width
            spacing: Style.space(6)
            property var device: modelData

            PanelSectionHeader {
              width: parent.width
              text: device.label + (device.status !== "online" ? " · unavailable" : "")
              foreground: Commons.Color.popups.text
            }

            Repeater {
              model: Model.characteristicSections(deviceSection.device)
              delegate: Column {
                required property var modelData
                width: deviceSection.width
                spacing: Style.space(4)

                Text {
                  visible: modelData.label !== null
                  width: parent.width
                  text: modelData.label || ""
                  color: Commons.Color.popups.text
                  font.family: Style.font.family
                  font.pixelSize: Style.font.bodySmall
                  font.bold: true
                }

                Repeater {
                  model: modelData.characteristics
                  delegate: Column {
                    id: characteristicRow
                    required property var modelData
                    width: deviceSection.width
                    spacing: Style.space(4)
                    property var characteristic: modelData
                    property string key: root.rowKey(deviceSection.device.id, characteristic.id)

                    Row {
                      width: parent.width
                      spacing: Style.space(8)
                      Text {
                        id: characteristicLabel
                        width: Math.min(Math.max(Style.space(100), parent.width * 0.48), parent.width)
                        text: characteristic.label
                        color: Commons.Color.popups.text
                        font.family: Style.font.family
                        font.pixelSize: Style.font.body
                        elide: Text.ElideRight
                      }
                      Text {
                        width: Math.max(0, parent.width - characteristicLabel.width - parent.spacing)
                        text: Model.formatValue(characteristic) + (root.connection !== "online" ? " · stale" : "")
                        color: Commons.Color.popups.text
                        font.family: Style.font.family
                        font.pixelSize: Style.font.bodySmall
                        elide: Text.ElideRight
                        horizontalAlignment: Text.AlignRight
                      }
                    }

                    Loader {
                      width: parent.width
                      sourceComponent: characteristic.spec.kind === "number" && Model.controlFor(characteristic) === "slider" ? sliderControl
                        : Model.controlFor(characteristic) === "stepper" ? stepperControl
                        : characteristic.spec.kind === "boolean" && Model.controlFor(characteristic) === "toggle" ? toggleControl
                        : characteristic.spec.kind === "enum" && Model.controlFor(characteristic) === "choice" ? choiceControl
                        : characteristic.spec.kind === "action" && Model.controlFor(characteristic) === "invoke" ? invokeControl
                        : Model.controlFor(characteristic) === "web" ? webControl
                        : null
                    }
                    Text {
                      visible: !!root.rowErrors[characteristicRow.key]
                      width: parent.width
                      text: root.rowErrors[characteristicRow.key] || ""
                      color: Commons.Color.urgent
                      wrapMode: Text.Wrap
                    }

                    Component {
                      id: sliderControl
                      Item {
                        width: characteristicRow.width
                        implicitHeight: slider.implicitHeight
                        PanelSlider {
                          id: slider
                          anchors.fill: parent
                          bar: root.bar
                          minimum: characteristicRow.characteristic.spec.minimum
                          maximum: characteristicRow.characteristic.spec.maximum
                          step: characteristicRow.characteristic.spec.step || (maximum - minimum) / 100
                          integer: characteristicRow.characteristic.spec.integer
                          value: typeof characteristicRow.characteristic.value === "number"
                            ? characteristicRow.characteristic.value : minimum
                          enabled: root.canCommand(deviceSection.device, characteristicRow.key)
                            && typeof characteristicRow.characteristic.value === "number"
                          onReleased: function(value) {
                            root.dispatch(deviceSection.device.id, characteristicRow.characteristic.id,
                              "set", Model.setArgument(characteristicRow.characteristic.spec, value));
                          }
                        }
                        MouseArea {
                          anchors.fill: parent
                          acceptedButtons: Qt.NoButton
                          onWheel: function(wheel) {
                            root.handleWheel(deviceSection.device, characteristicRow.characteristic, characteristicRow.key, wheel.angleDelta.y);
                            wheel.accepted = true;
                          }
                        }
                      }
                    }

                    Component {
                      id: stepperControl
                      Item {
                        width: characteristicRow.width
                        implicitHeight: buttons.implicitHeight
                        Row {
                          id: buttons
                          spacing: Style.space(8)
                          Button {
                            text: "−"
                            enabled: root.canCommand(deviceSection.device, characteristicRow.key)
                            onClicked: root.dispatch(deviceSection.device.id, characteristicRow.characteristic.id, "step", -1)
                          }
                          Button {
                            text: "+"
                            enabled: root.canCommand(deviceSection.device, characteristicRow.key)
                            onClicked: root.dispatch(deviceSection.device.id, characteristicRow.characteristic.id, "step", 1)
                          }
                        }
                        MouseArea {
                          anchors.fill: parent
                          acceptedButtons: Qt.NoButton
                          onWheel: function(wheel) {
                            root.handleWheel(deviceSection.device, characteristicRow.characteristic, characteristicRow.key, wheel.angleDelta.y);
                            wheel.accepted = true;
                          }
                        }
                      }
                    }

                    Component {
                      id: toggleControl
                      ToggleSwitch {
                        checked: characteristicRow.characteristic.value === true
                        interactive: root.canCommand(deviceSection.device, characteristicRow.key)
                        onToggled: root.dispatch(deviceSection.device.id, characteristicRow.characteristic.id, "toggle")
                      }
                    }

                    Component {
                      id: choiceControl
                      ButtonGroup {
                        options: characteristicRow.characteristic.spec.options.map(function(option) {
                          return { value: option.value, label: option.label };
                        })
                        value: typeof characteristicRow.characteristic.value === "string" ? characteristicRow.characteristic.value : ""
                        enabled: root.canCommand(deviceSection.device, characteristicRow.key)
                        onChanged: function(value) {
                          root.dispatch(deviceSection.device.id, characteristicRow.characteristic.id, "set", value);
                        }
                      }
                    }

                    Component {
                      id: invokeControl
                      Button {
                        text: "Invoke"
                        enabled: root.canCommand(deviceSection.device, characteristicRow.key)
                        onClicked: root.dispatch(deviceSection.device.id, characteristicRow.characteristic.id, "invoke")
                      }
                    }

                    Component {
                      id: webControl
                      Button {
                        text: "Edit in Web UI"
                        enabled: root.endpoint !== ""
                        onClicked: root.openWeb(deviceSection.device.id, characteristicRow.characteristic.id)
                      }
                    }
                  }
                }
              }
            }
          }
        }

        PanelSeparator { foreground: Commons.Color.popups.text }
        Button {
          text: "Open Web UI"
          enabled: root.endpoint !== ""
          onClicked: root.openWeb(null, null)
        }
      }
    }
  }
}
