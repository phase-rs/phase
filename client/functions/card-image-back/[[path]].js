import { proxyCardImage } from "../../deploy/cloudflare-pages/card-image-proxy.js";

export function onRequest(context) {
  return proxyCardImage(context, "https://backs.scryfall.io");
}
