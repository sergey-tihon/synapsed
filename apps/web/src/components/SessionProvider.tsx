import type { ParentProps } from "solid-js";
import { createSynapsedSession, SessionContext } from "../session";

export default function SessionProvider(props: ParentProps) {
  const session = createSynapsedSession();
  return <SessionContext value={session}>{props.children}</SessionContext>;
}
