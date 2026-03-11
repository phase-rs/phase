import type { GroupedPermanent as GroupedPermanentType } from "../../viewmodel/battlefieldProps";
import { PermanentCard } from "./PermanentCard.tsx";

interface GroupedPermanentProps {
  group: GroupedPermanentType;
}

export function GroupedPermanentDisplay({ group }: GroupedPermanentProps) {
  return <PermanentCard objectId={group.ids[0]} />;
}
