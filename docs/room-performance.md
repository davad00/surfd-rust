# Room server performance

The room server relays movement snapshots at a target of 20 Hz. The desktop runs the movement simulation at 66⅔ ticks per second, and ranked verification replays movement separately.

## How we measured

Native headless clients joined over TLS with signed-in identities and offered distinct movement poses at a 50 ms interval. Every listed test slot was occupied. Sampling started after an eight-second warmup. Sign-ins and ranked submissions ran alongside movement traffic. The records service verified native replay fixtures and returned save receipts.

Snapshot rate is the median receive rate per room, shown as a range across rooms. Snapshot gap is the time between roster arrivals. Pose age is the time from offering a sampled player's pose to receiving it in another client's roster, measured on the load generator's shared monotonic clock. It includes client queues, both network legs, and server scheduling. Displayed latency also depends on interpolation and rendering.

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

The run used about 3.8 times the sustained CPU budget and 7.3 times the sustained outbound budget. It ended at six minutes with burst reserve available. Four rooms of 16 were the largest tested layout that averaged within both sustained budgets.
