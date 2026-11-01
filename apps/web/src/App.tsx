import { createMemo, createSignal, onSettled, Show } from "solid-js";
import type { Accessor } from "solid-js";
import {
  href,
  parseRoute,
  DEFAULT_HARDWARE_FILTERS,
  type HardwareFilters,
  type LogLevelFilter,
  type Route,
} from "./router";
import { useSession } from "./session";
import SessionProvider from "./components/SessionProvider";
import Shell from "./components/Shell";
import Overview from "./pages/Overview";
import Hardware from "./pages/Hardware";
import Devices from "./pages/Devices";
import Device from "./pages/Device";
import Events from "./pages/Events";
import Logs from "./pages/Logs";
import NotFound from "./pages/NotFound";
import "./App.css";

interface AppContentProps {
  route: Accessor<Route>;
  updateRoute(route: Route): void;
}

function AppContent(props: AppContentProps) {
  const session = useSession();
  const hardwareFilters = createMemo(() => {
    const current = props.route();
    return current.page === "hardware" ? current.filters : DEFAULT_HARDWARE_FILTERS;
  });
  const deviceId = createMemo(() => {
    const current = props.route();
    return current.page === "device" ? current.deviceId : "";
  });
  const characteristicId = createMemo(() => {
    const current = props.route();
    return current.page === "device" ? current.characteristicId : null;
  });
  const logLevel = createMemo(() => {
    const current = props.route();
    return current.page === "logs" ? current.level : "all";
  });

  function updateHardwareFilters(filters: HardwareFilters) {
    props.updateRoute({ page: "hardware", filters });
  }
  function updateLogLevel(level: LogLevelFilter) {
    props.updateRoute({ page: "logs", level });
  }

  return (
    <Shell route={props.route()} connection={session.connection()}>
      <Show when={props.route().page === "overview"}>
        <Overview />
      </Show>
      <Show when={props.route().page === "hardware"}>
        <Hardware filters={hardwareFilters()} onFiltersChange={updateHardwareFilters} />
      </Show>
      <Show when={props.route().page === "devices"}>
        <Devices />
      </Show>
      <Show when={props.route().page === "device"}>
        <Device deviceId={deviceId()} characteristicId={characteristicId()} />
      </Show>
      <Show when={props.route().page === "events"}>
        <Events />
      </Show>
      <Show when={props.route().page === "logs"}>
        <Logs level={logLevel()} onLevelChange={updateLogLevel} />
      </Show>
      <Show when={props.route().page === "not-found"}>
        <NotFound />
      </Show>
    </Shell>
  );
}

export default function App() {
  const [hash, setHash] = createSignal("#/");
  const route = createMemo(() => parseRoute(hash()));

  function updateRoute(next: Route) {
    if (typeof window === "undefined") return;
    window.history.replaceState(null, "", href(next));
    setHash(window.location.hash || "#/");
  }

  onSettled(() => {
    if (typeof window === "undefined") return;
    const update = () => setHash(window.location.hash || "#/");
    window.addEventListener("hashchange", update);
    update();
    return () => window.removeEventListener("hashchange", update);
  });

  return (
    <SessionProvider>
      <AppContent route={route} updateRoute={updateRoute} />
    </SessionProvider>
  );
}
