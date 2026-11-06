import { useEffect, useState } from "react";
import { Action, ActionPanel, getPreferenceValues, List, openExtensionPreferences } from "@raycast/api";
import { characteristicSections } from "@synapsed/client";
import CharacteristicForm, { CommandActions } from "./characteristic-form";
import { bindingKey, displayValue, Session, useSession } from "./session";

export default function Devices() {
  const { endpoint } = getPreferenceValues<{ endpoint: string }>();
  const [session] = useState(() => new Session());
  useEffect(() => {
    session.start(endpoint);
    return () => session.close();
  }, [session, endpoint]);
  const state = useSession(session);
  const snapshot = state.snapshot;
  const drivers = snapshot?.drivers ?? state.status?.drivers ?? [];
  const node = snapshot?.node ?? state.status?.node;
  const stale = state.connection !== "connected";
  const preferences = <Action title="Open Extension Preferences" onAction={openExtensionPreferences} />;
  return (
    <List
      isShowingDetail
      isLoading={state.connection === "connecting" && !snapshot}
      searchBarPlaceholder="Search devices and characteristics"
    >
      <List.Section title="Local Service" subtitle={state.connection}>
        <List.Item
          id="service"
          title={node?.name ?? "Synapsed"}
          subtitle={stale ? `${state.connection} · values stale` : "Connected"}
          detail={
            <List.Item.Detail
              metadata={
                <List.Item.Detail.Metadata>
                  <List.Item.Detail.Metadata.Label title="Connection" text={state.connection} />
                  <List.Item.Detail.Metadata.Label title="Node ID" text={node?.nodeId ?? "Not received"} />
                  <List.Item.Detail.Metadata.Label title="Instance ID" text={node?.instanceId ?? "Not received"} />
                  <List.Item.Detail.Metadata.Label
                    title="API Version"
                    text={String(snapshot?.apiVersion ?? state.status?.apiVersion ?? "Not received")}
                  />
                  <List.Item.Detail.Metadata.Label title="Revision" text={snapshot?.revision ?? "Not received"} />
                  <List.Item.Detail.Metadata.Label title="Endpoint" text={endpoint} />
                  <List.Item.Detail.Metadata.Label title="Error" text={state.error ?? "None"} />
                </List.Item.Detail.Metadata>
              }
            />
          }
          actions={<ActionPanel>{preferences}</ActionPanel>}
        />
        {drivers.map((driver) => (
          <List.Item
            key={driver.id}
            id={`driver:${driver.id}`}
            title={driver.id}
            subtitle={driver.state}
            detail={
              <List.Item.Detail
                metadata={
                  <List.Item.Detail.Metadata>
                    <List.Item.Detail.Metadata.Label title="Version" text={driver.version} />
                    <List.Item.Detail.Metadata.Label title="State" text={driver.state} />
                    <List.Item.Detail.Metadata.Label title="Error" text={driver.error?.message ?? "None"} />
                  </List.Item.Detail.Metadata>
                }
              />
            }
            actions={
              <ActionPanel>
                <Action
                  title={state.restartingDrivers.has(driver.id) ? "Restarting Driver" : "Restart Driver"}
                  onAction={() => void session.restartDriver(driver.id)}
                />
                {preferences}
              </ActionPanel>
            }
          />
        ))}
      </List.Section>
      {snapshot?.devices.length === 0 && (
        <List.Section title="Devices">
          <List.Item
            id="empty"
            title="No devices declared"
            subtitle="Check installed drivers and hardware availability"
            actions={<ActionPanel>{preferences}</ActionPanel>}
          />
        </List.Section>
      )}
      {snapshot?.devices.map((device) =>
        characteristicSections(device.groups, device.characteristics).map((section) => (
          <List.Section
            key={`${device.id}:${section.group?.id ?? ""}`}
            title={section.group ? `${device.label} · ${section.group.label}` : device.label}
            subtitle={`${device.kind} · ${device.status}${stale ? " · Stale" : ""}`}
          >
            {section.characteristics.length === 0 && (
              <List.Item id={`empty:${device.id}`} title="No characteristics declared" />
            )}
            {section.characteristics.map((characteristic) => {
              const pending = state.pending.has(bindingKey(device.id, characteristic.id));
              const enabled = !stale && device.status === "online" && !pending;
              return (
                <List.Item
                  key={characteristic.id}
                  id={bindingKey(device.id, characteristic.id)}
                  title={characteristic.label}
                  subtitle={displayValue(characteristic)}
                  accessories={[
                    {
                      text: pending ? "Pending" : `${characteristic.status}${stale ? " · Stale" : ""}`,
                    },
                  ]}
                  detail={
                    <List.Item.Detail
                      metadata={
                        <List.Item.Detail.Metadata>
                          <List.Item.Detail.Metadata.Label title="Device" text={`${device.label} (${device.id})`} />
                          <List.Item.Detail.Metadata.Label title="Driver" text={device.driver} />
                          <List.Item.Detail.Metadata.Label title="Device status" text={device.status} />
                          <List.Item.Detail.Metadata.Label
                            title="Device error"
                            text={device.error?.message ?? "None"}
                          />
                          <List.Item.Detail.Metadata.Separator />
                          <List.Item.Detail.Metadata.Label title="Characteristic ID" text={characteristic.id} />
                          <List.Item.Detail.Metadata.Label title="Observation" text={displayValue(characteristic)} />
                          <List.Item.Detail.Metadata.Label
                            title="Observation status"
                            text={`${characteristic.status}${stale ? " (stale)" : ""}`}
                          />
                          <List.Item.Detail.Metadata.Label
                            title="Error"
                            text={characteristic.error?.message ?? "None"}
                          />
                          <List.Item.Detail.Metadata.Label
                            title="Specification"
                            text={JSON.stringify(characteristic.spec)}
                          />
                          <List.Item.Detail.Metadata.Label
                            title="Access"
                            text={`read=${characteristic.access.read}, write=${characteristic.access.write}, notify=${characteristic.access.notify}`}
                          />
                        </List.Item.Detail.Metadata>
                      }
                    />
                  }
                  actions={
                    <ActionPanel>
                      <Action.Push
                        title={
                          characteristic.access.write && characteristic.spec.kind !== "action"
                            ? "Edit Characteristic"
                            : "Inspect Characteristic"
                        }
                        target={
                          <CharacteristicForm
                            session={session}
                            deviceId={device.id}
                            characteristicId={characteristic.id}
                          />
                        }
                      />
                      <CommandActions
                        session={session}
                        deviceId={device.id}
                        characteristic={characteristic}
                        enabled={enabled}
                      />
                      {preferences}
                    </ActionPanel>
                  }
                />
              );
            })}
          </List.Section>
        )),
      )}
    </List>
  );
}
