import type { ApplicationConnectionStore } from "../ApplicationConnectionStore";
import { RuntimeConnectionForm } from "../RuntimeConnectionForm";
import { RuntimeEntryLayout } from "../RuntimeEntryLayout";

type DesktopEntryPageProps = {
  readonly connection: ApplicationConnectionStore;
};

export function DesktopEntryPage({ connection }: DesktopEntryPageProps) {
  return (
    <RuntimeEntryLayout description="选择一个 Runtime，继续你的工作。">
      <RuntimeConnectionForm connection={connection} />
    </RuntimeEntryLayout>
  );
}
