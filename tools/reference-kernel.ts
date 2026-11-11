import { resolve } from "node:path";

const MAX_REFERENCE_COMMANDS = 100_000;
const REPLAY_LIMIT = 24 * 1024 * 1024;
const GEOMETRY_LIMIT = 192 * 1024 * 1024;
const MIRROR = Bun.env.SURF_REFERENCE_MIRROR ?? resolve(import.meta.dir, "../../surfd-offline-clone/offline-clone");

type Point = { x: number; y: number; z: number };
export type ReferenceConfig = {
  version: string;
  dt: number;
  autoBhop: boolean;
  unrestrictedStart: boolean;
  [key: string]: unknown;
};
export type ReferenceCommand = {
  forward: number;
  side: number;
  jump: boolean;
  duck: boolean;
  yaw: number;
  pitch: number;
  restartStage?: boolean;
};
export type ReferenceReplay = {
  format: number;
  mapId: string;
  mapVersion: string;
  config: ReferenceConfig;
  initialState: unknown;
  commands: ReferenceCommand[];
};
export type ReferenceState = {
  position: Point;
  velocity: Point;
  yaw: number;
  pitch: number;
  tick: number;
  eyeHeight: number;
  ducked: boolean;
};
export type ReferenceTimer = {
  phase: "ready" | "running" | "finished";
  startTick: number | null;
  endTick: number | null;
  splits: number[];
};
export type ReferenceGame = {
  state: ReferenceState;
  timer: { state: ReferenceTimer };
  checkpointSpeeds: number[];
  step(command: ReferenceCommand): string[];
  loadReplay(replay: ReferenceReplay): void;
};
type CatalogEntry = { id: string; slug: string; name: string; version: string; config: ReferenceConfig };
type MapData = { id: string; version: string; [key: string]: unknown };
type BonusBoard = { id: string; version: string; geometrySha256: string; bonus: { fileSha256: string } };

function bonusIdentity(version: string) {
  const match = /^(.+)\/bonus-([1-9]\d*)(?:\.r[1-9]\d*)?$/.exec(version);
  return match ? { mainVersion: match[1], number: Number(match[2]) } : undefined;
}

async function referenceWorker() {
  const workers = Array.from(new Bun.Glob("ghost.worker-*.js").scanSync({
    cwd: resolve(MIRROR, "assets"), onlyFiles: true,
  }));
  if (workers.length !== 1) throw new Error("Expected one local Surfd ghost worker in the mirror");
  return Bun.file(resolve(MIRROR, "assets", workers[0])).text();
}

async function readBounded(stream: ReadableStream<Uint8Array>, limit: number) {
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > limit) {
        await reader.cancel();
        throw new Error(`Input exceeds ${limit} bytes`);
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

export async function readReferenceReplay(argument: string): Promise<ReferenceReplay> {
  const file = Bun.file(argument);
  let bytes: Uint8Array;
  if (await file.exists()) {
    bytes = await readBounded(file.stream(), REPLAY_LIMIT);
  } else if (/^[a-z0-9_-]{1,64}\/bonus-[1-9]\d{0,4}$/.test(argument)) {
    const route = bonusIdentity(argument)!;
    const worker = await referenceWorker();
    const json = worker.match(/JSON\.parse\('((?:\[\{"mapId":"surf_demise").*?\}\])'\)/)?.[1];
    if (!json) throw new Error("The local Surfd bonus catalog is unavailable");
    const entries = JSON.parse(json) as { slug: string; bonuses: { number: number; replaySha256?: string }[] }[];
    const hash = entries.find((entry) => entry.slug === route.mainVersion)?.bonuses.find((bonus) => bonus.number === route.number)?.replaySha256;
    if (!hash || !/^[0-9a-f]{64}$/.test(hash)) throw new Error("No original record is published for this bonus");
    const response = await fetch(`https://surfd-assets.classic-surf.workers.dev/packed/${hash}.json.gz`, {
      signal: AbortSignal.timeout(30_000), redirect: "error",
    });
    if (!response.ok || !response.body) throw new Error(`Surfd replay download returned HTTP ${response.status}`);
    bytes = await readBounded(response.body, REPLAY_LIMIT);
    if (bytes[0] === 31 && bytes[1] === 139) {
      bytes = await readBounded(new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip")), REPLAY_LIMIT);
    }
    if (new Bun.CryptoHasher("sha256").update(bytes).digest("hex") !== hash) {
      throw new Error("The original bonus replay checksum does not match");
    }
  } else {
    if (!/^[a-z0-9_-]{1,64}$/.test(argument)) {
      throw new Error("Use a local replay file or a Surfd map slug");
    }
    const response = await fetch(`https://surfd.net/replays/${argument}-complete.json`, {
      signal: AbortSignal.timeout(30_000), redirect: "error",
    });
    if (!response.ok || !response.body) {
      throw new Error(`Surfd replay download returned HTTP ${response.status}`);
    }
    bytes = await readBounded(response.body, REPLAY_LIMIT);
  }
  const replay = JSON.parse(new TextDecoder().decode(bytes)) as ReferenceReplay;
  if (!replay || !Number.isInteger(replay.format) || replay.format < 1 || replay.format > 8 ||
    typeof replay.mapId !== "string" || typeof replay.mapVersion !== "string" ||
    !Array.isArray(replay.commands) || replay.commands.length === 0 ||
    replay.commands.length > MAX_REFERENCE_COMMANDS ||
    !Number.isFinite(replay.config?.dt) || replay.config.dt <= 0 || replay.config.dt > 0.1 ||
    typeof replay.initialState !== "object" || !replay.initialState) {
    throw new Error("Invalid Surfd command replay");
  }
  const invalid = replay.commands.findIndex((command) => !command ||
    !Number.isFinite(command.forward) || !Number.isFinite(command.side) ||
    !Number.isFinite(command.yaw) || !Number.isFinite(command.pitch) ||
    typeof command.jump !== "boolean" || typeof command.duck !== "boolean" ||
    (command.restartStage !== undefined && typeof command.restartStage !== "boolean"));
  if (invalid !== -1) throw new Error(`Invalid reference command at index ${invalid}`);
  return replay;
}

async function readGeometry(path: string) {
  const file = Bun.file(path);
  const magic = new Uint8Array(await file.slice(0, 2).arrayBuffer());
  const stream = magic[0] === 0x1f && magic[1] === 0x8b
    ? file.stream().pipeThrough(new DecompressionStream("gzip")) : file.stream();
  return readBounded(stream, GEOMETRY_LIMIT);
}

/** Load the reference bundle once, exposing its simulation constructor and map loader. */
export async function loadReferenceKernel(geometryPath: string, mapId: string, version?: string, bonusPackPath?: string) {
  const config = await Bun.file(resolve(MIRROR, "config.json")).json() as { maps: CatalogEntry[]; bonusBoards?: BonusBoard[] };
  const entry = config.maps.find((map) => map.id === mapId);
  if (!entry) throw new Error("Replay map is absent from the local Surfd catalog");
  const geometryBytes = await readGeometry(geometryPath);
  const raw = JSON.parse(new TextDecoder().decode(geometryBytes)) as MapData;
  const requiredVersion = version ? bonusIdentity(version)?.mainVersion ?? version : undefined;
  if (raw.id !== mapId || (requiredVersion !== undefined && raw.version !== requiredVersion)) {
    throw new Error(`Replay needs ${mapId} geometry at revision ${version ?? entry.version}`);
  }
  const geometrySha256 = new Bun.CryptoHasher("sha256").update(geometryBytes).digest("hex");
  const bonusBytes = bonusPackPath ? await readGeometry(bonusPackPath) : undefined;
  const bonusPackSha256 = bonusBytes ? new Bun.CryptoHasher("sha256").update(bonusBytes).digest("hex") : undefined;
  const bonusPack = bonusBytes ? JSON.parse(new TextDecoder().decode(bonusBytes)) : undefined;
  if (bonusPack && (bonusPack.mapId !== mapId || bonusPack.mapVersion !== raw.version)) {
    throw new Error("The bonus pack belongs to another map revision");
  }
  const allowed = new Set([`/packed/${geometrySha256}.json.gz`, `/maps/${entry.slug}-collision.json`]);
  const localFetch = (input: string | URL | Request) => {
    const requested = input instanceof Request ? input.url : input.toString();
    const path = new URL(requested, "https://surfd.net").pathname;
    if (!allowed.has(path)) throw new Error(`The reference worker needs another local asset: ${path}`);
    return Promise.resolve(new Response(geometryBytes));
  };
  const worker = await referenceWorker();
  const replayLoader = worker.indexOf("loadReplay(");
  const beforeLoader = worker.slice(0, replayLoader);
  const gameClass = Array.from(beforeLoader.matchAll(/class ([$\w]+)\{constructor\(/g)).at(-1)?.[1];
  const mapLoader = worker.match(/\.mapVersion===\w+\.currentVersion&&\(\w+=await ([$\w]+)\(\w+\.entry\)/)?.[1];
  const bonusLoader = worker.match(/=([$\w]+)\(\w+,await [$\w]+\(\w+\),\w+\.number\),\w+\.version!==\w+\.mapVersion\)throw Error\("This bonus run was recorded on an earlier revision of the bonus"/)?.[1];
  const end = worker.lastIndexOf("})();");
  if (!gameClass || !mapLoader || (bonusPack && !bonusLoader) || end < 0) throw new Error("The local Surfd worker layout has changed");
  type Scope = {
    desktop?: {
      create: (map: MapData, rules: ReferenceConfig, format: number) => ReferenceGame;
      load: (entry: CatalogEntry) => Promise<MapData>;
      bonus?: (map: MapData, pack: unknown, number: number) => MapData;
    };
    postMessage(): void;
  };
  const scope: Scope = { postMessage() {} };
  const bridge = `;self.desktop={create:(map,rules,format)=>new ${gameClass}(map,rules,format),load:${mapLoader},bonus:${bonusLoader ?? "undefined"}};`;
  new Function("self", "fetch", worker.slice(0, end) + bridge + worker.slice(end))(scope, localFetch);
  if (!scope.desktop) throw new Error("The local Surfd worker did not initialize");
  // Current maps use the exact loader. Archived maps are already decoded from
  // their supplied collision file and must have passed the identity check above.
  const map = raw.version === entry.version ? await scope.desktop.load(entry) : raw;
  const desktop = scope.desktop;
  return {
    geometrySha256,
    bonusPackSha256,
    create: (rules: ReferenceConfig, format: number) => desktop.create(map, rules, format),
    createForReplay: (replay: ReferenceReplay) => {
      const route = bonusIdentity(replay.mapVersion);
      if (!route) {
        if (replay.mapId !== map.id || replay.mapVersion !== map.version) throw new Error("Replay geometry revision does not match");
        return desktop.create(map, replay.config, replay.format);
      }
      if (!bonusPack || !desktop.bonus) throw new Error("Supply the original bonus pack for this replay");
      const board = config.bonusBoards?.find((board) => board.id === replay.mapId && board.version === replay.mapVersion);
      if (board?.geometrySha256 !== geometrySha256 || board?.bonus.fileSha256 !== bonusPackSha256) {
        throw new Error("The bonus replay and assets do not match the catalog revision");
      }
      const course = desktop.bonus(map, bonusPack, route.number);
      if (course.version !== replay.mapVersion) throw new Error("The bonus replay belongs to another route revision");
      return desktop.create(course, replay.config, replay.format);
    },
  };
}
