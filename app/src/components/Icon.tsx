import {
  AlertTriangle,
  Archive,
  ArchiveRestore,
  ArrowRight,
  BadgeCheck,
  Bot,
  Box,
  Check,
  ChevronLeft,
  ChevronRight,
  CircleDashed,
  CircleDot,
  Clock,
  Cloud,
  Columns2,
  CornerDownLeft,
  Drama,
  Eye,
  EyeOff,
  FileText,
  FileVideo,
  Flag,
  Gauge,
  GitBranch,
  Hand,
  HelpCircle,
  Hourglass,
  Inbox,
  Library,
  Link,
  Lock,
  MessageSquare,
  Package,
  PauseCircle,
  Pencil,
  Radar,
  Radio,
  Redo2,
  RefreshCw,
  Scan,
  Search,
  Server,
  Settings,
  Shield,
  SquareDashed,
  Star,
  Target,
  Terminal,
  Timer,
  Undo2,
  User,
  X,
  ZoomIn,
  ZoomOut,
} from "lucide-react";

/**
 * Icon vocabulary: one icon, one meaning, named Lucide imports,
 * fixed sizes and strokes. There is no emoji fallback and no string lookup
 * outside this exhaustive map.
 */
const ICONS = {
  "git-branch": GitBranch,
  package: Package,
  "circle-dashed": CircleDashed,
  "alert-triangle": AlertTriangle,
  eye: Eye,
  "message-square": MessageSquare,
  "help-circle": HelpCircle,
  "undo-2": Undo2,
  hourglass: Hourglass,
  lock: Lock,
  clock: Clock,
  timer: Timer,
  gauge: Gauge,
  shield: Shield,
  bot: Bot,
  user: User,
  check: Check,
  x: X,
  flag: Flag,
  "badge-check": BadgeCheck,
  drama: Drama,
  "arrow-right": ArrowRight,
  "refresh-cw": RefreshCw,
  "pause-circle": PauseCircle,
  pencil: Pencil,
  archive: Archive,
  "archive-restore": ArchiveRestore,
  "eye-off": EyeOff,
  "file-text": FileText,
  "file-video": FileVideo,
  search: Search,
  "chevron-left": ChevronLeft,
  "chevron-right": ChevronRight,
  "corner-down-left": CornerDownLeft,
  inbox: Inbox,
  target: Target,
  star: Star,
  link: Link,
  radar: Radar,
  radio: Radio,
  server: Server,
  settings: Settings,
  cloud: Cloud,
  terminal: Terminal,
  hand: Hand,
  "columns-2": Columns2,
  box: Box,
  "circle-dot": CircleDot,
  library: Library,
  "square-dashed": SquareDashed,
  "zoom-in": ZoomIn,
  "zoom-out": ZoomOut,
  scan: Scan,
  "redo-2": Redo2,
} as const;

export type IconName = keyof typeof ICONS;

export const ICON_NAMES = Object.keys(ICONS) as IconName[];

/** Project glyphs are identity, not status: any other icon string is rejected. */
export const PROJECT_GLYPHS = [
  "radar",
  "radio",
  "box",
  "columns-2",
  "circle-dot",
  "library",
  "file-text",
  "server",
  "cloud",
  "terminal",
] as const;

export function isProjectGlyph(value: string | undefined): value is IconName {
  return !!value && (PROJECT_GLYPHS as readonly string[]).includes(value);
}

export function isIconName(value: string | undefined): value is IconName {
  return !!value && Object.prototype.hasOwnProperty.call(ICONS, value);
}

interface IconProps {
  name: IconName;
  className?: string;
  size?: number;
  strokeWidth?: number;
}

export function Icon({ name, className, size = 17, strokeWidth = 1.9 }: IconProps) {
  const Glyph = ICONS[name];
  return (
    <Glyph
      className={className ? `ic ${className}` : "ic"}
      size={size}
      strokeWidth={strokeWidth}
      aria-hidden="true"
      focusable={false}
    />
  );
}
