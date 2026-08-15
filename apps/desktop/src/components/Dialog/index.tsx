import {
  useEffect,
  useRef,
  type PropsWithChildren,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";

type DialogProps = PropsWithChildren<Readonly<{
  aria_describedby?: string;
  aria_label?: string;
  aria_labelledby?: string;
  backdrop_class_name: string;
  dialog_class_name: string;
  dismissible?: boolean;
  initial_focus_ref?: RefObject<HTMLElement | null>;
  on_close: () => void;
}>>;

const modal_stack: symbol[] = [];
const focusable_selector = [
  "a[href]",
  "button:not(:disabled)",
  "input:not(:disabled)",
  "select:not(:disabled)",
  "textarea:not(:disabled)",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

export function Dialog(props: DialogProps) {
  const dialog_ref = useRef<HTMLElement>(null);
  const opener_ref = useRef<HTMLElement | null>(null);
  const instance_ref = useRef(Symbol("dialog"));
  const on_close_ref = useRef(props.on_close);
  const dismissible_ref = useRef(props.dismissible ?? true);
  on_close_ref.current = props.on_close;
  dismissible_ref.current = props.dismissible ?? true;

  useEffect(() => {
    const instance = instance_ref.current;
    const dialog = dialog_ref.current;
    if (!dialog) {
      return undefined;
    }
    const dialog_element: HTMLElement = dialog;

    opener_ref.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    modal_stack.push(instance);
    const focus_frame = requestAnimationFrame(() => {
      const initial_focus = props.initial_focus_ref?.current ?? getFocusableElements(dialog_element)[0] ?? dialog_element;
      initial_focus.focus();
    });

    function handleKeyDown(event: KeyboardEvent) {
      if (modal_stack.at(-1) !== instance) {
        return;
      }
      if (event.key === "Escape" && dismissible_ref.current) {
        event.preventDefault();
        event.stopPropagation();
        on_close_ref.current();
        return;
      }
      if (event.key !== "Tab") {
        return;
      }

      const elements = getFocusableElements(dialog_element);
      if (elements.length === 0) {
        event.preventDefault();
        dialog_element.focus();
        return;
      }
      const first = elements[0];
      const last = elements[elements.length - 1];
      const active = document.activeElement;
      if (event.shiftKey && (active === first || !dialog_element.contains(active))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (active === last || !dialog_element.contains(active))) {
        event.preventDefault();
        first.focus();
      }
    }

    document.addEventListener("keydown", handleKeyDown, true);
    return () => {
      cancelAnimationFrame(focus_frame);
      document.removeEventListener("keydown", handleKeyDown, true);
      const stack_index = modal_stack.lastIndexOf(instance);
      if (stack_index >= 0) {
        modal_stack.splice(stack_index, 1);
      }
      if (opener_ref.current?.isConnected) {
        opener_ref.current.focus();
      }
      opener_ref.current = null;
    };
  }, [props.initial_focus_ref]);

  const overlay_root = document.querySelector<HTMLElement>("#overlay-root") ?? document.body;
  return createPortal(
    <div
      className={props.backdrop_class_name}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && dismissible_ref.current) {
          on_close_ref.current();
        }
      }}
    >
      <section
        aria-describedby={props.aria_describedby}
        aria-label={props.aria_label}
        aria-labelledby={props.aria_labelledby}
        aria-modal="true"
        className={props.dialog_class_name}
        ref={dialog_ref}
        role="dialog"
        tabIndex={-1}
      >
        {props.children}
      </section>
    </div>,
    overlay_root,
  );
}

function getFocusableElements(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(focusable_selector))
    .filter((element) => element.getAttribute("aria-hidden") !== "true" && !element.hidden);
}
