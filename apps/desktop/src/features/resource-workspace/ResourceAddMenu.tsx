import { useEffect, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { Icon } from "../../components/Icon";
import { createResourceMenu } from "../../native-bridge/resourceMenu";

/** 系统菜单可直接覆盖原生子 WebView，打开时不需要隐藏网页。 */
export function ResourceAddMenu(props: Readonly<{
  workspace_available: boolean;
  terminal_available?: boolean;
  on_terminal?: () => void;
  on_workspace: () => void;
  on_browser: () => void;
  on_error: (failure: unknown) => void;
}>) {
  const callbacks = useRef(props);
  callbacks.current = props;
  const menu = useRef<ReturnType<typeof createResourceMenu> | null>(null);
  const mounted = useRef(true);
  const opening = useRef(false);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      const retained = menu.current;
      menu.current = null;
      if (retained) void retained.then((value) => value.dispose()).catch((failure: unknown) => callbacks.current.on_error(failure));
    };
  }, []);

  return <Button aria-expanded={open} aria-haspopup="menu" aria-label="新建资源标签" iconOnly size="small" variant="text"
    onClick={(event) => {
      if (opening.current) return;
      const bounds = event.currentTarget.getBoundingClientRect();
      const anchor = { x: bounds.left, y: bounds.bottom + 4 };
      opening.current = true;
      setOpen(true);
      menu.current ??= createResourceMenu(
        () => { if (mounted.current) callbacks.current.on_workspace(); },
        () => { if (mounted.current) callbacks.current.on_browser(); },
        () => { if (mounted.current) callbacks.current.on_terminal?.(); },
      ).catch((failure: unknown) => { menu.current = null; throw failure; });
      void menu.current.then(async (value) => {
        if (mounted.current) await value.popup(callbacks.current.workspace_available, anchor, callbacks.current.terminal_available ?? false);
      }).catch((failure: unknown) => {
        callbacks.current.on_error(failure);
      }).finally(() => {
        opening.current = false;
        if (mounted.current) setOpen(false);
      });
    }}><Icon name="plus" size={17} /></Button>;
}
