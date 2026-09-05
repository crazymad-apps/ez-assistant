import {
  forwardRef,
  type ButtonHTMLAttributes,
} from "react";
import styles from "./index.module.scss";

export type ButtonVariant = "text" | "outlined" | "primary" | "danger";
export type ButtonSize = "small" | "default" | "large";

export type ButtonVisualProps = Readonly<{
  className?: string;
  iconOnly?: boolean;
  size?: ButtonSize;
  variant?: ButtonVariant;
}>;

export type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & ButtonVisualProps;

export function buttonVisualProps(props: ButtonVisualProps) {
  return {
    className: [styles.button, props.className].filter(Boolean).join(" "),
    "data-button-icon-only": props.iconOnly || undefined,
    "data-button-variant": props.variant ?? "outlined",
    "data-size": props.size ?? "default",
  } as const;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(props, ref) {
  const {
    className,
    iconOnly,
    size,
    type = "button",
    variant,
    ...button_props
  } = props;

  return (
    <button
      {...button_props}
      {...buttonVisualProps({ className, iconOnly, size, variant })}
      ref={ref}
      type={type}
    />
  );
});
