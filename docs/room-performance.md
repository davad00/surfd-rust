# Room server performance

The room server relays movement snapshots at a target of 20 Hz. The desktop runs movement at the course's pinned interval. Most courses use 15 ms per tick, and Jaunt 3, 4, and 6 use 10 ms. Ranked verification replays movement separately.

## Public rooms

The public directory offers 88 slots across three rooms. The catalog currently has maps through T7.

| Slots | Rotation | Maps in rotation |
| ---: | --- | ---: |
| 64 | T1–T2 | 52 |
| 16 | T3–T5 | 11 |
| 8 | T6+ | 8 |

## How we measured

Native headless clients joined over TLS with signed-in identities and offered distinct movement poses at a 50 ms interval. Every listed test slot was occupied. Sampling started after an eight-second warmup. Sign-ins and ranked submissions ran alongside movement traffic. The records service verified native replay fixtures and returned save receipts.

Snapshot rate is the median receive rate per room, shown as a range across rooms. Snapshot gap is the time between roster arrivals. Pose age is the time from offering a sampled player's pose to receiving it in another client's roster, measured on the load generator's shared monotonic clock. It includes client queues, both network legs, and server scheduling. Displayed latency also depends on interpolation and rendering.

## v0.1.2 room relay

The server batches socket reads on Linux, waits for connections when a room is empty, and keeps the 20 Hz snapshot schedule. Updated desktop clients receive smaller, lossless binary rosters. Earlier clients still use compressed JSON with a more efficient compression setting. Both formats carry the same movement data. Local simulation and ranked replay verification use their existing formats.

Each 15-minute activity run used native signed-in clients moving simultaneously in three isolated rooms. Three groups of 12 sign-ins and 12 ranked saves ran near the start, middle, and end. Each run also included nine planned disconnects and rejoins, one majority map vote per room, and four signed ticket renewals per client.

| Players | Room sizes | Client format | Mean guest CPU | Mean outbound | Min. available RAM | Movement age p95 |
| ---: | --- | --- | ---: | ---: | ---: | ---: |
| 88 | 64 + 16 + 8 | Compressed JSON | 9.72% | 44.62 Mb/s | 279 MiB | 99 ms |
| 112 | 64 + 32 + 16 | Compact binary | 10.71% | 33.45 Mb/s | 278 MiB | 97 ms |

These two preliminary runs used 10 ms socket checks. Both completed all activity at 20.00 Hz room medians with zero unexpected disconnects. Over ten fully occupied minutes, the legacy-format run used about 0.47 net CPU credits and the 112-player compact run used about 0.17. The instance meter includes CPU work outside the guest OS, so the guest CPU percentage alone cannot establish sustained capacity.

### Sustained capacity with the updated desktop

The final build batches established socket checks on Linux and retains the 50 ms snapshot schedule. Empty rooms wait for connections. The 104-player test filled the 64-slot room and two additional tiered rooms on the same machine. Both the guest measurements and the instance credit meter were collected during the run.

| Players | Room sizes | Mean guest CPU | Metered CPU | Mean outbound | Min. available RAM | Movement age p95 / p99 | Net credits over 10 min |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 88 | 64 + 16 + 8 | 3.83% | 7.62% | 27.51 Mb/s | 278 MiB | 95 / 99 ms | +0.48 |
| 104 | 64 + 24 + 16 | 7.09% | 8.46% | 30.78 Mb/s | 275 MiB | 95 / 98 ms | +0.31 |

Metered CPU is calculated from credit use in two complete five-minute intervals within each 15-minute run. The 104-player run retained about 1.54 percentage points of the 10% CPU budget and 33.22 Mb/s of the 64 Mb/s outbound budget. Outbound traffic for the 88-slot layout dropped from 64.39 Mb/s to 27.51 Mb/s. Both runs kept 20.00 Hz room medians and had zero unexpected disconnects, out-of-memory events, swap-outs, or network allowance drops.

Both runs completed 36 sign-ins and 36 ranked saves. Sign-in p95 was 591 ms at 88 players and 600 ms at 104. Ranked save p95 was 1.98 s and 2.35 s from attempt to receipt. All map votes reached every client, and full movement rosters resumed within 151 ms of the first map change notification. The measurements describe the two tested room layouts with the updated desktop client.

## Full public layout before v0.1.2

All 88 slots in the 64 + 16 + 8 layout stayed occupied by moving, signed-in clients for 180 seconds. Every client received 20.00 snapshots per second. The run had zero unexpected disconnects and completed all 16 sign-ins and 12 ranked saves under full load.

| Snapshot gap p95 | Pose age p95 / p99 | Sign-in p95 | Ranked save p95 |
| ---: | ---: | ---: | ---: |
| 52.85 ms | 102 / 109 ms | 433 ms | 1.44 s |

| Mean CPU | Peak five-second CPU | Mean outbound | Minimum available RAM |
| ---: | ---: | ---: | ---: |
| 11.63% | 32.97% | 64.39 Mb/s | 451 MiB |

The host counters recorded zero out-of-memory events, swap-outs, and network allowance drops. Full occupancy used burst capacity. The final minute averaged 10.65% CPU and 64.06 Mb/s outbound, still slightly above the sustained baselines of 10% CPU and 64 Mb/s. The six-minute 80-player result below averaged within both guest CPU and bandwidth baselines.

## Results

Gap and age percentiles show the highest per-room value. Every run in the table kept all listed clients connected throughout its measurement window.

| Players | Room sizes | Window | Snapshot rate | Gap p95 | Pose age p95 | Pose age p99 | Ranked saves |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 32 | 2 × 16 | 145 s | 20.00 Hz | 51.17 ms | 95 ms | 101 ms | 32/32 |
| 64 | 4 × 16 | 155 s | 20.00 Hz | 52.97 ms | 99 ms | 104 ms | 16/16 |
| 80 | 5 × 16 | 215 s | 20.00 Hz | 52.75 ms | 100 ms | 104 ms | 16/16 |
| 128 | 2 × 64 | 70 s | 20.00 Hz | 52.71 ms | 103 ms | 110 ms | 12/12 |
| 256 | 4 × 64 | 65 s | 20.00 Hz | 52.67 ms | 104 ms | 109 ms | 16/16 |
| 384 | 6 × 64 | 65 s | 20.00 Hz | 53.11 ms | 104 ms | 110 ms | 16/16 |
| 512 | 8 × 64 | 60 s | 19.98–20.00 Hz | 53.60 ms | 102 ms | 111 ms | 16/16 |
| 640 | 10 × 64 | 55 s | 19.98–20.00 Hz | 54.24 ms | 105 ms | 111 ms | 16/16 |
| 768 | 12 × 64 | 50 s | 19.96–20.00 Hz | 54.90 ms | 106 ms | 115 ms | 12/12 |

## Burst and sustained load

The test machine had two vCPUs and 1 GiB RAM. Its rated sustained baselines were 10% aggregate CPU and 64 Mb/s outbound. CPU percentages below are shares of total CPU capacity. Available memory is the lowest sampled amount during each run.

| Players | Mean CPU | Mean outbound | Minimum available RAM |
| ---: | ---: | ---: | ---: |
| 32 | 7.68% | 9.10 Mb/s | 412 MiB |
| 64 | 9.42% | 18.36 Mb/s | 220 MiB |
| 80 | 10.90% | 22.82 Mb/s | 223 MiB |
| 512 | 47.84% | 471.88 Mb/s | 230 MiB |
| 768 | 60.87% | 707.50 Mb/s | 236 MiB |

The 32- and 64-player runs fit both sustained baselines on average in their measured windows. The 80-player run exceeded the CPU baseline. The 512- and 768-player results used burst capacity. CPU averaged 42.47% in the final 30 seconds of the original 60-second run at 512 players and reached 90.9% in one five-second sample during the 768-player ramp.

Larger rooms send larger rosters to more clients. At 80 players, two rooms of 40 averaged 48.30 Mb/s outbound. Five rooms of 16 averaged 22.82 Mb/s. The timing results describe steady-map play on the measured network path.

## Six-minute 512-player run

Eight rooms held 64 moving clients each for 360 seconds with zero disconnects. All 512 clients renewed their signed-in identity on the existing connection and continued past the initial five-minute lease. During the full-room window, 64 sign-ins and 12 ranked saves succeeded. Sign-in took 1.56 s at p95. Ranked saves took 4.40 s at p95 from attempt to receipt.

| Window | Room median snapshot rate | Gap p95 | Pose age p95 | Pose age p99 |
| --- | ---: | ---: | ---: | ---: |
| Full run | 20.00 Hz | 53.59 ms | 102 ms | 109 ms |
| First minute | 20.00 Hz | 53.69 ms | 102 ms | 110 ms |
| Last minute | 20.00 Hz | 53.61 ms | 102 ms | 109 ms |

The machine averaged 38.04% CPU and 465.70 Mb/s outbound. Peak five-second CPU was 55.08%, and minimum available RAM was 274 MiB. The host samples recorded zero out-of-memory events, swap-outs, and network allowance drops.

The run used about 3.8 times the sustained CPU budget and 7.3 times the sustained outbound budget. It ended at six minutes with burst reserve available.

## Mixed room sizes

Two later six-minute runs used the same class of machine and filled every listed slot with a moving, signed-in client. All clients stayed connected through the measurement window and through a renewal of their room tickets. Each run also completed 16 sign-ins and 12 ranked saves under full load.

| Players | Room sizes | Snapshot rate | Gap p95 | Pose age p95 / p99 | Sign-in p95 | Ranked save p95 |
| ---: | --- | ---: | ---: | ---: | ---: | ---: |
| 80 | 64 + 16 | 20.00 Hz | 53.17 ms | 102 / 108 ms | 416 ms | 1.32 s |
| 96 | 64 + 16 + 16 | 20.00 Hz | 52.85 ms | 100 / 108 ms | 432 ms | 1.33 s |

| Players | Mean CPU | Peak five-second CPU | Mean outbound | Minimum available RAM |
| ---: | ---: | ---: | ---: | ---: |
| 80 | 8.85% | 11.61% | 62.89 Mb/s | 419 MiB |
| 96 | 11.33% | 32.34% | 67.40 Mb/s | 424 MiB |

The 80-player layout averaged within both guest CPU and bandwidth baselines over the measurement window, with 1.11 Mb/s of outbound headroom. The 96-player layout used burst capacity. Its final minute still averaged 11.30% CPU and 67.04 Mb/s outbound. The 768-player result above is the highest completed brief test in this report.

## Four-room capacity checks

Four follow-up runs filled four rooms simultaneously. Each run completed 16 sign-ins and 12 ranked saves with zero unexpected disconnects, out-of-memory events, or network allowance drops.

| Players | Room sizes | Window | Snapshot rate | Gap p95 | Pose age p95 / p99 | Mean CPU | Mean outbound | Minimum available RAM |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 64 | 4 × 16 | 155 s | 20.00 Hz | 52.99 ms | 97 / 104 ms | 8.62% | 18.24 Mb/s | 338 MiB |
| 128 | 4 × 32 | 155 s | 19.99–20.00 Hz | 52.66 ms | 96 / 105 ms | 15.20% | 64.31 Mb/s | 299 MiB |
| 144 | 64 + 32 + 16 + 16 | 150 s | 20.00 Hz | 52.58 ms | 100 / 106 ms | 13.76% | 83.93 Mb/s | 447 MiB |
| 256 | 4 × 64 | 120 s | 19.99–20.00 Hz | 52.62 ms | 103 / 109 ms | 24.70% | 233.31 Mb/s | 289 MiB |

## Reconnects and voted map changes

A separate two-minute run started with 96 moving, signed-in clients across three tiered rotations. Nine clients intentionally disconnected and rejoined, with six abrupt drops and three clean leaves. Each rejoin used a fresh room ticket and the same account. The rosters returned to full in 1.15–1.21 s, including a deliberate one-second wait. There were zero unexpected disconnects.

| Slots | Tiers tested | Votes needed | Clients notified | Full movement roster after first notification |
| ---: | --- | ---: | ---: | ---: |
| 64 | T1–T2 | 33 | 64/64 | 167 ms |
| 16 | T3–T5 | 9 | 16/16 | 149 ms |
| 16 | T6–T7 | 9 | 16/16 | 110 ms |

The recovery measurements cover server notifications and movement relays to headless clients. Desktop map loading also depends on cached assets, downloads, and rendering. The test recorded 20.00 Hz room medians, a 53.14 ms worst room p95 snapshot gap, and 98 ms worst room p95 pose age. Sixteen sign-ins and 12 ranked saves succeeded. The machine averaged 13.25% CPU and 67.56 Mb/s outbound, with a minimum of 319 MiB available RAM.

## Source map loading in the records service

The records service skips render-only BSP lumps when it needs collision and timing data. The table reports median process CPU time across three paired warm-cache trials on Windows. The change affects map cache misses. The measurements cover loading and leave the room's steady snapshot budget unmeasured.

| Map workload | Loads per trial | Previous CPU time | Updated CPU time |
| --- | ---: | ---: | ---: |
| Two BSPs with internally compressed render lumps | 24 | 7,406 ms | 5,391 ms |
| Seven mostly uncompressed CS:S BSPs | 56 | 24,219 ms | 23,875 ms |
| Seven CS:S BSPs, six wrapped in gzip | 28 | 16,125 ms | 16,141 ms |

Collision brushes, triggers, and 900 movement ticks matched across eight tested maps. An isolated ARM records service saved six archived command runs at their expected times, including a Surf course with 18 stage records and all three 100 Hz Jaunts. Boards, profiles, replays, and save receipts persisted across restart. A second account submitting a copied trajectory with a camera-only change was rejected.
