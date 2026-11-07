import type { ReactNode } from "react";
import type { BlobRef, ProjectSummary } from "../api/types";
import { useBlob } from "../hooks/useBlob";
import { mediaUrl } from "../state/delivery";
import { Icon, isProjectGlyph } from "./Icon";

interface ProjectAvatarProps {
  glyph: string | undefined;
  size?: "sm" | "md" | "lg";
  /** The project, so its own icon (set by an agent or a human) can show. */
  project?: ProjectSummary | undefined;
}

/**
 * Identity tile. The project's own icon shows in full colour, as drawn (the
 * user's choice); without one, a monochrome glyph: shape is the identity,
 * never a status.
 */
export function ProjectAvatar({ glyph, size = "md", project }: ProjectAvatarProps) {
  const name = isProjectGlyph(glyph) ? glyph : "radar";
  const classes = ["av"];
  if (size === "sm") classes.push("sm");
  if (size === "lg") classes.push("lg");
  const fallback = <Icon name={name} strokeWidth={size === "lg" ? 1.7 : 2} />;
  return (
    <span className={classes.join(" ")} aria-hidden="true">
      {project?.icon ? (
        <ProjectIcon slug={project.slug} icon={project.icon} fallback={fallback} />
      ) : (
        fallback
      )}
    </span>
  );
}

/** Mounted only when an icon exists, so avatars without one never read blobs. */
function ProjectIcon({ slug, icon, fallback }: { slug: string; icon: BlobRef; fallback: ReactNode }) {
  const view = useBlob(slug, icon);
  if (view.phase !== "ready" || !view.token) return <>{fallback}</>;
  return <img className="av-img" src={mediaUrl(view.token)} alt="" draggable={false} />;
}
