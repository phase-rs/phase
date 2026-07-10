import type { GameObject, ObjectId } from "../adapter/types.ts";

function isObjectAttachedToHost(object: GameObject, hostId: ObjectId): boolean {
  return object.attached_to?.type === "Object" && object.attached_to.data === hostId;
}

export function activeAttachmentIds(
  objects: Record<string, GameObject> | undefined,
  host: GameObject | undefined,
): ObjectId[] {
  if (!host) return [];
  return host.attachments.filter((id) => {
    const attachment = objects?.[id];
    return (
      attachment != null
      && attachment.zone === "Battlefield"
      && isObjectAttachedToHost(attachment, host.id)
    );
  });
}
