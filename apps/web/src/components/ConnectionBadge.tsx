import type { ClientStatus } from "@synapsed/client";

interface Props {
  status: ClientStatus;
}

export default function ConnectionBadge(props: Props) {
  const label = () => {
    switch (props.status) {
      case "connected":
        return "Online";
      case "disconnected":
        return "Offline · reconnecting";
      case "incompatible":
        return "Incompatible";
      case "connecting":
        return "Connecting";
    }
  };
  return (
    <span class="connection-badge" data-state={props.status} role="status">
      <span class="connection-dot" aria-hidden="true" />
      {label()}
    </span>
  );
}
