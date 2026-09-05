import {
  forwardRef,
  type ButtonHTMLAttributes,
} from "react";
import { Icon, type IconName } from "../Icon";
import styles from "./index.module.scss";

export type InlineIconButtonProps = Omit<
  ButtonHTMLAttributes<HTMLButtonElement>,
  "aria-label" | "children"
> & Readonly<{
  icon: IconName;
  label: string;
  size?: number;
}>;

export const InlineIconButton = forwardRef<HTMLButtonElement, InlineIconButtonProps>(
  function InlineIconButton(props, ref) {
    const {
      className,
      icon,
      label,
      size = 16,
      type = "button",
      ...button_props
    } = props;

    return (
      <button
        {...button_props}
        aria-label={label}
        className={[styles.button, className].filter(Boolean).join(" ")}
        ref={ref}
        type={type}
      >
        <Icon name={icon} size={size} />
      </button>
    );
  },
);
