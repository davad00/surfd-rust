import { dirname, resolve } from "node:path";
import { mkdir } from "node:fs/promises";

const REPLAY_LIMIT = 24 * 1024 * 1024;
const GEOMETRY_LIMIT = 192 * 1024 * 1024;
const WORKER_TIMEOUT_MS = 180_000;

type Replay = {
  mapId: string;
  mapVersion: string;
  commands: unknown[];
};

type Track = {
  mapId: string;
  mapVersion: string;
  format: number;
  dt: number;
  count: number;
  positions: Float32Array;
  yaws: Float32Array;
  flags: Uint8Array;
  startTick: number;
  endTick: number;
  finished: boolean;
};

type WorkerScope = {
  onmessage?: (message: { data: unknown }) => void;
  postMessage: (message: { ok: true; track: Track } | { ok: false; error: string }) => void;
};

async function readBounded(stream: ReadableStream<Uint8Array>, limit: number) {
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > limit) {
        await reader.cancel();
        throw new Error(`Input exceeds the ${limit}-byte size limit`);
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

async function readReplay(argument: string): Promise<Replay> {
  const file = Bun.file(argument);
  if (await file.exists()) {
    return JSON.parse(new TextDecoder().decode(await readBounded(file.stream(), REPLAY_LIMIT)));
  }
  if (!/^[a-z0-9_-]{1,64}$/.test(argument)) {
    throw new Error("Use a local replay file or a Surfd map slug");
  }
  const url = `https://surfd.net/replays/${argument}-complete.json`;
  const response = await fetch(url, { signal: AbortSignal.timeout(30_000), redirect: "error" });
  if (!response.ok || !response.body) {
    throw new Error(`Surfd replay download returned HTTP ${response.status}`);
  }
  return JSON.parse(new TextDecoder().decode(await readBounded(response.body, REPLAY_LIMIT)));
}

async function readGeometry(argument: string) {
  const file = Bun.file(argument);
  const magic = new Uint8Array(await file.slice(0, 2).arrayBuffer());
  const stream = magic[0] === 0x1f && magic[1] === 0x8b
    ? file.stream().pipeThrough(new DecompressionStream("gzip"))
    : file.stream();
  return readBounded(stream, GEOMETRY_LIMIT);
}

async function convert(record: string, geometry: string, output: string) {
  const mirror = Bun.env.SURF_REFERENCE_MIRROR ?? resolve(import.meta.dir, "../../surfd-offline-clone/offline-clone");
  const workerNames = Array.from(
    new Bun.Glob("ghost.worker-*.js").scanSync({ cwd: resolve(mirror, "assets"), onlyFiles: true }),
  );
  if (workerNames.length !== 1) {
    throw new Error("Expected one local Surfd ghost worker in the offline mirror");
  }
  const config = await Bun.file(resolve(mirror, "config.json")).json() as {
    maps: { id: string; slug: string; name: string; version: string }[];
  };
  const replay = await readReplay(record);
  if (!Array.isArray(replay.commands) || replay.commands.length === 0 || replay.commands.length > 40_000) {
    throw new Error("Reference replay must contain between 1 and 40,000 commands");
  }
  const entry = config.maps.find(map => map.id === replay.mapId);
  if (!entry) throw new Error("Replay map is absent from the local Surfd catalog");

  const geometryBytes = await readGeometry(geometry);
  const map = JSON.parse(new TextDecoder().decode(geometryBytes)) as { id: string; version: string };
  if (map.id !== replay.mapId || map.version !== replay.mapVersion) {
    throw new Error(`Replay needs ${replay.mapId} geometry at revision ${replay.mapVersion}`);
  }
  const geometrySha256 = new Bun.CryptoHasher("sha256").update(geometryBytes).digest("hex");
  const packedPath = `/packed/${geometrySha256}.json.gz`;
  const plainPath = `/maps/${entry.slug}-collision.json`;
  const localFetch = (input: string | URL | Request) => {
    const requested = input instanceof Request ? input.url : input.toString();
    const path = new URL(requested, "https://surfd.net").pathname;
    if (path !== packedPath && path !== plainPath) {
      throw new Error(`The worker needs another local asset: ${path}`);
    }
    return Promise.resolve(new Response(geometryBytes));
  };

  const worker = await Bun.file(resolve(mirror, "assets", workerNames[0])).text();
  const track = await new Promise<Track>((resolveTrack, rejectTrack) => {
    const timer = setTimeout(() => rejectTrack(new Error("Ghost conversion timed out")), WORKER_TIMEOUT_MS);
    const scope: WorkerScope = {
      postMessage(message) {
        clearTimeout(timer);
        if (message.ok) resolveTrack(message.track);
        else rejectTrack(new Error(message.error));
      },
    };
    try {
      // The local mirrored worker supplies the original simulation. Its only
      // fetches are served from the caller's matching local collision asset.
      new Function("self", "fetch", worker)(scope, localFetch);
      if (!scope.onmessage) throw new Error("The local ghost worker did not initialize");
      scope.onmessage({ data: { entry, currentVersion: entry.version, replay } });
    } catch (error) {
      clearTimeout(timer);
      rejectTrack(error);
    }
  });
  if (!track.finished || track.mapId !== map.id || track.mapVersion !== map.version) {
    throw new Error("The original worker did not produce a finished track for this map");
  }
  const trajectory = {
    version: 1,
    mapId: track.mapId,
    mapVersion: track.mapVersion,
    geometrySha256,
    dt: track.dt,
    startTick: track.startTick,
    endTick: track.endTick,
    positions: Array.from(track.positions),
    yaws: Array.from(track.yaws),
    flags: Array.from(track.flags),
  };
  await mkdir(dirname(resolve(output)), { recursive: true });
  await Bun.write(output, JSON.stringify(trajectory));
  console.log(`Imported ${track.mapId} ghost with ${track.count} samples to ${output}`);
}

const [record, geometry, output, extra] = Bun.argv.slice(2);
if (!record || !geometry || !output || extra) {
  console.error("Usage: bun tools/import-reference-ghost.ts <slug-or-replay.json> <collision.json.gz> <output.json>");
  process.exit(2);
}
try {
  await convert(record, geometry, output);
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
