import { loadReferenceKernel, readReferenceReplay } from "./reference-kernel";

async function convert(record: string, geometry: string, output?: string, bonusPack?: string) {
  const replay = await readReferenceReplay(record);
  const kernel = await loadReferenceKernel(geometry, replay.mapId, replay.mapVersion, bonusPack);
  const game = kernel.createForReplay(replay);
  game.loadReplay(replay);

  const positions: number[] = [];
  const velocities: number[] = [];
  const yaws: number[] = [];
  const pitches: number[] = [];
  const eyeHeights: number[] = [];
  const flags: number[] = [];
  const inputs: number[] = [0];
  const capture = (discontinuity: boolean) => {
    const { position, velocity, yaw, pitch, eyeHeight, ducked } = game.state;
    positions.push(position.x, position.y, position.z);
    velocities.push(velocity.x, velocity.y, velocity.z);
    yaws.push(yaw);
    pitches.push(pitch);
    eyeHeights.push(eyeHeight);
    flags.push(Number(ducked) | (discontinuity ? 2 : 0));
  };

  capture(false);
  let startTick: number | null = null;
  let endTick: number | null = null;
  for (const [index, command] of replay.commands.entries()) {
    const offset = index - game.state.tick;
    const { x, y, z } = game.state.position;
    const events = game.step(command);
    inputs.push((command.forward > 0 ? 1 : command.forward < 0 ? 2 : 0) |
      (command.side > 0 ? 4 : command.side < 0 ? 8 : 0) |
      (command.jump ? 16 : 0) | (command.duck ? 32 : 0));
    const next = game.state.position;
    capture(events.includes("teleport") || events.includes("fall") ||
      events.includes("stage-restarted") || Math.hypot(next.x - x, next.y - y, next.z - z) > 600);
    const timer = game.timer.state;
    startTick = timer.phase === "ready" || timer.startTick === null ? null : timer.startTick + offset;
    if (timer.phase === "finished" && timer.endTick !== null) {
      endTick = timer.endTick + offset;
      break;
    }
  }
  if (startTick === null || endTick === null) {
    throw new Error("The recorded commands did not finish a timed route");
  }

  const track = JSON.stringify({
    version: 1,
    mapId: replay.mapId,
    mapVersion: replay.mapVersion,
    geometrySha256: kernel.geometrySha256,
    bonusPackSha256: kernel.bonusPackSha256,
    dt: replay.config.dt,
    startTick,
    endTick,
    positions,
    yaws,
    flags,
    view: {
      pitches,
      eyeHeights,
      velocities,
      splits: game.timer.state.splits,
      checkpointSpeeds: game.checkpointSpeeds,
      inputs,
    },
  });
  await Bun.write(output ?? Bun.stdout, track);
}

const [record, geometry, ...args] = Bun.argv.slice(2);
const bonusIndex = args.indexOf("--bonus-pack");
const bonusPack = bonusIndex >= 0 ? args.splice(bonusIndex, 2)[1] : undefined;
if (!record || !geometry || args.length > 1 || (bonusIndex >= 0 && !bonusPack)) {
  console.error("Usage: bun tools/import-reference-replay.ts <slug-or-replay.json> <collision.json.gz> [output.json] [--bonus-pack PACK]");
  process.exit(2);
}
try {
  await convert(record, geometry, args[0], bonusPack);
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
