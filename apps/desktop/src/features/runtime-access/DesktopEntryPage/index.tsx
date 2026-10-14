import { LoginOptionsDialog } from "../LoginOptionsDialog";
import type { ApplicationConnectionStore } from "../ApplicationConnectionStore";
import { RuntimeConnectionForm } from "../RuntimeConnectionForm";
import { RuntimeEntryLayout } from "../RuntimeEntryLayout";

type DesktopEntryPageProps = {
  readonly connection: ApplicationConnectionStore;
};

export function DesktopEntryPage({ connection }: DesktopEntryPageProps) {
  return (
    <RuntimeEntryLayout>
      <RuntimeConnectionForm connection={connection} />
      <LoginOptionsDialog connection={connection} />
    </RuntimeEntryLayout>
  );
}
