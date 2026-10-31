import type { ParentProps } from "solid-js";
import type { ClientStatus } from "@synapsed/client";
import type { Route } from "../router";
import ConnectionBadge from "./ConnectionBadge";

interface Props extends ParentProps {
  route: Route;
  connection: ClientStatus;
}

const navigation: { page: "overview" | "hardware" | "devices" | "logs"; label: string; href: string }[] = [
  { page: "overview", label: "Overview", href: "#/" },
  { page: "hardware", label: "Hardware", href: "#/hardware" },
  { page: "devices", label: "Devices", href: "#/devices" },
  { page: "logs", label: "Logs", href: "#/logs" },
];

function activePage(route: Route, page: (typeof navigation)[number]["page"]): boolean {
  return route.page === page || (page === "devices" && route.page === "device");
}

export default function Shell(props: Props) {
  return (
    <div class="app-shell">
      <aside class="sidebar">
        <a class="brand" href="#/" aria-label="Synapsed overview">
          <img src="/favicon.svg" alt="" width="40" height="40" />
          <span>Synapsed</span>
        </a>
        <nav aria-label="Main navigation">
          {navigation.map((item) => (
            <a href={item.href} aria-current={activePage(props.route, item.page) ? "page" : undefined}>
              <span class={`nav-icon nav-icon--${item.page}`} aria-hidden="true" />
              {item.label}
            </a>
          ))}
        </nav>
        <p class="sidebar-note">Local devices. Confirmed state.</p>
      </aside>
      <div class="main-column">
        <header class="topbar">
          <span class="mobile-brand">
            <img src="/favicon.svg" alt="" width="32" height="32" />
            Synapsed
          </span>
          <ConnectionBadge status={props.connection} />
          <nav class="mobile-navigation" aria-label="Main navigation">
            {navigation.map((item) => (
              <a href={item.href} aria-current={activePage(props.route, item.page) ? "page" : undefined}>
                {item.label}
              </a>
            ))}
          </nav>
        </header>
        <main>{props.children}</main>
        <footer>Loopback-only client. Hardware access stays with the daemon and installed drivers.</footer>
      </div>
    </div>
  );
}
