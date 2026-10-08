import { ArenaBackground, type ArenaBackgroundProps } from "./ArenaBackground.tsx";

export function OceanTempleBackground(props: Omit<ArenaBackgroundProps, "color">) {
  return <ArenaBackground {...props} color="Blue" />;
}
