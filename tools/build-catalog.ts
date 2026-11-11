import { readdir } from "node:fs/promises";
import { resolve } from "node:path";

type MapConfig = {
  id: string;
  slug: string;
  geometrySha256: string;
};

const project = resolve(import.meta.dir, "..");
const mirror = Bun.env.SURF_REFERENCE_MIRROR ?? resolve(project, "../surfd-offline-clone/offline-clone");
const assetDir = resolve(mirror, "assets");
const bundleNames = (await readdir(assetDir)).filter((name) => /^game-.+\.js$/.test(name));
if (bundleNames.length !== 1) {
  throw new Error("Expected exactly one game bundle in the local Surfd mirror.");
}

const config = (await Bun.file(resolve(mirror, "config.json")).json()) as {
  maps: MapConfig[];
  bonusBoards: { id: string; bonus: { fileSha256: string } }[];
};
const source = await Bun.file(resolve(assetDir, bundleNames[0])).text();
const origin = source.match(/https:\/\/surfd-assets\.[a-z.-]+/)?.[0];
if (!origin || !config.maps.length) {
  throw new Error("Missing Surfd map metadata or public asset origin.");
}

const metadata = new Map<string, { title: string; author: string; tier: number; staged: boolean }>();
for (const match of source.matchAll(/\{id:"(surf_[a-z\d_]+)",slug:"[a-z\d-]+",name:"[^"]+",title:/g)) {
  const fields = source.slice(match.index + match[0].length, source.indexOf("}", match.index));
  const title = fields.match(/^("(?:[^"\\]|\\.)*")/)?.[1];
  const author = fields.match(/,author:("(?:[^"\\]|\\.)*")/)?.[1];
  const tier = fields.match(/,tier:(\d+)/)?.[1];
  if (title && author && tier) {
    metadata.set(match[1], {
      title: JSON.parse(title),
      author: JSON.parse(author),
      tier: Number(tier),
      staged: /,stages:[1-9]\d*/.test(fields),
    });
  }
}

const aliases = new Map<string, string>();
for (const [, alias, path] of source.matchAll(/([\w$]+)="(\/packed\/[a-f\d]{64}\.visual-manifest\.gz)"/g)) {
  aliases.set(alias, path);
}
const visualMap = [...source.matchAll(/\{[^{}]+\}/g)].find(([entry]) =>
  entry.includes("boreas:") && entry.includes("kitsune:") && entry.includes(".visual-manifest.gz"),
)?.[0];
if (!visualMap) {
  throw new Error("Missing the visual manifest table in the local Surfd bundle.");
}

const visuals = new Map<string, string>();
for (const [, quoted, bare, path, alias] of visualMap.matchAll(/(?:"([\w-]+)"|([\w$]+)):(?:"(\/packed\/[a-f\d]{64}\.visual-manifest\.gz)"|([\w$]+))/g)) {
  const slug = quoted ?? bare;
  const resolved = path ?? aliases.get(alias);
  if (!resolved) {
    throw new Error(`Unknown visual manifest alias for ${slug}.`);
  }
  visuals.set(slug, resolved);
}

const bonuses = new Map<string, { count: number; hash: string }>();
for (const board of config.bonusBoards) {
  const hash = board.bonus.fileSha256;
  const existing = bonuses.get(board.id);
  if (!/^[a-f\d]{64}$/.test(hash) || (existing && existing.hash !== hash)) {
    throw new Error(`Invalid or inconsistent bonus pack for ${board.id}.`);
  }
  bonuses.set(board.id, { count: (existing?.count ?? 0) + 1, hash });
}

const catalog = config.maps.map((map) => {
  const entry = metadata.get(map.id);
  if (!entry || !/^[a-f\d]{64}$/.test(map.geometrySha256)) {
    throw new Error(`Incomplete catalog data for ${map.id}.`);
  }
  const visual = visuals.get(map.slug);
  const bonus = bonuses.get(map.id);
  return {
    id: map.id,
    slug: map.slug,
    name: entry.title,
    author: entry.author,
    tier: entry.tier,
    type: entry.staged ? "staged" : "linear",
    geometry: `${origin}/packed/${map.geometrySha256}.json.gz`,
    visual: visual ? origin + visual : null,
    bonuses: bonus?.count ?? 0,
    bonusPack: bonus ? `${origin}/packed/${bonus.hash}.json.gz` : null,
  };
});

const output = "[\n" + catalog.map((map) => "  " + JSON.stringify(map)).join(",\n") + "\n]\n";
await Bun.write(resolve(project, "assets/surfd-catalog.json"), output);
console.log(`Wrote ${catalog.length} catalog entries with ${catalog.filter((map) => map.visual).length} visual manifests.`);
