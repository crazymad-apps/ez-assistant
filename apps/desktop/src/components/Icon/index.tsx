import type { ReactNode, SVGProps } from "react";

export type IconName =
  | "archive"
  | "arrow-down"
  | "bot"
  | "check"
  | "chevron-down"
  | "chevron-up"
  | "chevron-left"
  | "chevron-right"
  | "copy"
  | "edit"
  | "folder"
  | "fork"
  | "menu"
  | "message"
  | "more"
  | "paperclip"
  | "pin"
  | "plus"
  | "refresh"
  | "search"
  | "settings"
  | "sidebar-left"
  | "sidebar-right"
  | "shield"
  | "stop"
  | "terminal"
  | "trash"
  | "thumb-down"
  | "thumb-up"
  | "x";

export type IconProps = Readonly<
  SVGProps<SVGSVGElement> & {
    name: IconName;
    size?: number;
  }
>;

const paths: Record<IconName, ReactNode> = {
  archive: <><rect x="4" y="5" width="16" height="15" rx="2" /><path d="M3 5h18V3H3zM9 10h6" /></>,
  "arrow-down": <><path d="M12 4v15M6 13l6 6 6-6" /></>,
  bot: <><rect x="4" y="7" width="16" height="12" rx="3" /><path d="M12 3v4M8 12h.01M16 12h.01M8 16h8" /></>,
  check: <path d="m5 12 4 4L19 6" />,
  "chevron-down": <path d="m7 10 5 5 5-5" />,
  "chevron-up": <path d="m7 14 5-5 5 5" />,
  "chevron-left": <path d="m15 18-6-6 6-6" />,
  "chevron-right": <path d="m9 18 6-6-6-6" />,
  copy: <><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M15 9V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h3" /></>,
  edit: <><path d="m4 20 4.2-1 10-10a2.1 2.1 0 0 0-3-3l-10 10L4 20Z" /><path d="m13.7 7.5 3 3" /></>,
  folder: <path d="M3 7.5A2.5 2.5 0 0 1 5.5 5H9l2 2h7.5A2.5 2.5 0 0 1 21 9.5v7A2.5 2.5 0 0 1 18.5 19h-13A2.5 2.5 0 0 1 3 16.5z" />,
  fork: <><circle cx="6" cy="5" r="2" /><circle cx="18" cy="5" r="2" /><circle cx="12" cy="19" r="2" /><path d="M6 7v2c0 3 2 4 6 4s6-1 6-4V7M12 13v4" /></>,
  menu: <><path d="M4 7h16M4 12h16M4 17h16" /></>,
  message: (
    <g fill="currentColor" stroke="none" transform="scale(0.0234375)">
      <path d="M341.333 405.333h341.334c12.8 0 21.333-8.533 21.333-21.333s-8.533-21.333-21.333-21.333H341.333C328.533 362.667 320 371.2 320 384s10.667 21.333 21.333 21.333zM341.333 533.333h256c12.8 0 21.334-8.533 21.334-21.333s-8.534-21.333-21.334-21.333h-256C328.533 490.667 320 499.2 320 512s10.667 21.333 21.333 21.333z" />
      <path d="M957.867 448c0-211.2-200.534-381.867-445.867-381.867C264.533 66.133 64 236.8 64 448c0 108.8 66.133 217.6 181.333 294.4 10.667 6.4 23.467 4.267 29.867-6.4s4.267-23.467-6.4-29.867C209.067 665.6 106.667 578.133 106.667 448c0-187.733 181.333-339.2 405.333-339.2 221.867 0 403.2 151.467 403.2 339.2S733.867 787.2 512 787.2c-2.133 0-4.267 0-6.4 2.133-10.667-4.266-21.333 2.134-25.6 12.8-8.533 25.6-70.4 66.134-125.867 96 23.467-76.8 12.8-89.6 8.534-96-4.267-6.4-12.8-10.666-21.334-10.666-12.8 0-21.333 8.533-21.333 21.333 0 6.4 2.133 10.667 6.4 14.933 0 17.067-14.933 66.134-27.733 104.534-2.134 8.533 0 17.066 6.4 23.466 4.266 4.267 8.533 6.4 14.933 6.4 2.133 0 6.4 0 8.533-2.133 25.6-10.667 151.467-68.267 185.6-128C759.467 829.867 957.867 659.2 957.867 448z" />
    </g>
  ),
  more: <><circle cx="5" cy="12" r="1" fill="currentColor" stroke="none" /><circle cx="12" cy="12" r="1" fill="currentColor" stroke="none" /><circle cx="19" cy="12" r="1" fill="currentColor" stroke="none" /></>,
  paperclip: <path d="m20 11-8.5 8.5a5 5 0 0 1-7-7L14 3a3.5 3.5 0 0 1 5 5l-9.5 9.5a2 2 0 0 1-3-3L15 6" />,
  pin: (
    <path
      d="M334.182 616.09l73.728 73.728c-72.601 61.44-178.636 156.928-200.55 174.08C151.706 907.57 102.4 921.6 102.4 921.6s14.029-49.254 57.702-104.96c17.152-21.914 112.64-127.898 174.08-200.55zM905.932 315.392 708.66 118.067a53.402 53.402 0 1 0-75.673 75.623l15.718 15.718L360.55 412.979l-63.795-63.795a53.402 53.402 0 1 0-75.622 75.622l378.01 377.959a53.402 53.402 0 1 0 75.622-75.623l-63.693-63.692 203.52-288.256 15.718 15.718a53.402 53.402 0 1 0 75.623-75.571z"
      fill="currentColor"
      stroke="none"
      transform="scale(0.0234375)"
    />
  ),
  plus: <path d="M12 5v14M5 12h14" />,
  refresh: <><path d="M20 7v5h-5" /><path d="M18.1 16A7 7 0 1 1 19.6 9L20 12" /></>,
  search: <><circle cx="11" cy="11" r="6.5" /><path d="m16 16 4 4" /></>,
  settings: <><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-2.8 2.8-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.6v.2h-4V21a1.7 1.7 0 0 0-1-1.6 1.7 1.7 0 0 0-1.9.3l-.1.1L4.2 17l.1-.1a1.7 1.7 0 0 0 .3-1.9A1.7 1.7 0 0 0 3 14H2.8v-4H3a1.7 1.7 0 0 0 1.6-1 1.7 1.7 0 0 0-.3-1.9L4.2 7 7 4.2l.1.1a1.7 1.7 0 0 0 1.9.3A1.7 1.7 0 0 0 10 3v-.2h4V3a1.7 1.7 0 0 0 1 1.6 1.7 1.7 0 0 0 1.9-.3l.1-.1L19.8 7l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.6 1h.2v4H21a1.7 1.7 0 0 0-1.6 1Z" /></>,
  "sidebar-left": <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M9 4v16" /></>,
  "sidebar-right": <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M15 4v16" /></>,
  shield: <path d="M12 3 5 6v5c0 4.6 2.8 8 7 10 4.2-2 7-5.4 7-10V6z" />,
  stop: <rect x="7" y="7" width="10" height="10" rx="1.5" fill="currentColor" stroke="none" />,
  terminal: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="m7 9 3 3-3 3M13 15h4" /></>,
  trash: <><path d="M4 7h16M9 7V4h6v3M7 7l1 13h8l1-13M10 11v5M14 11v5" /></>,
  "thumb-down": <path d="M7 4v10H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h3Zm0 9 4 7a2 2 0 0 0 3-2v-4h4.4a2 2 0 0 0 2-2.4L19 6a2 2 0 0 0-2-2H7" />,
  "thumb-up": <path d="M7 20V10H4a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h3Zm0-9 4-7a2 2 0 0 1 3 2v4h4.4a2 2 0 0 1 2 2.4L19 18a2 2 0 0 1-2 2H7" />,
  x: <path d="m7 7 10 10M17 7 7 17" />,
};

export function Icon(props: IconProps) {
  const { name, size = 18, ...svg_props } = props;
  return (
    <svg
      aria-hidden="true"
      data-icon={name}
      fill="none"
      height={size}
      stroke="currentColor"
      strokeLinecap="round"
      strokeLinejoin="round"
      strokeWidth="1.8"
      viewBox="0 0 24 24"
      width={size}
      {...svg_props}
    >
      {paths[name]}
    </svg>
  );
}
