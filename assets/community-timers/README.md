# Community Surf timer credits

The following timer sidecars use the authored zones and configurations from [wrldspawn’s Surf zone archive](https://github.com/wrldspawn/surf-zones/tree/51025635e19b833c00bb69c88559cea087434e0d) under the included [Unlicense](LICENSE). They are pinned to specific Source BSP revisions.

- **Surf 1998**, by grizzlix. The sidecar includes the authored stage spawns and the archive’s trigger fix. [Original release](https://gamebanana.com/mods/545237)
- **Surf 2pacisalive**, by umg_. [Map and tier credit](https://ksf.surf/maps/surf_2pacisalive)
- **Surf School**, by sid. The sidecar includes the archive’s autobhop setting for both routes. [Map and tier credit](https://ksf.surf/maps/surf_school_fix)
- **Calycate**, by ArcticPanda. [Map and tier credit](https://ksf.surf/maps/surf_calycate_ksf)
- **Satellite**, by biji and iNooVa. The sidecar opts into the original map's gravity triggers. [Map and tier credit](https://ksf.surf/maps/surf_satellite_fix)
- **Surf 004 Fix**, by monkeywithagun. This uses the archive's two-checkpoint Shavit route. [Map and tier credit](https://ksf.surf/maps/surf_004_fix)
- **Surf 6**, by vay. [Map and tier credit](https://ksf.surf/maps/surf_6)
- **Surf Mom Fix**. [Map and tier credit](https://ksf.surf/maps/surf_mom_fix)
- **Surf Water Run**. [Map and tier credit](https://ksf.surf/maps/surf_water_run_ksf)

The desktop fetches the original BSPs from [FastDL](https://fastdl.me/#questions). These sidecars contain timing data only.

## Simpsons Source

The desktop Practice course uses the ten-stage main route from [SurfTimer's published zone and tier tables](https://github.com/surftimer/SurfTimer/tree/b9c860942b8a55b7e36c244264f53c4e6d13a32b/scripts/mysql-files). The original Counter-Strike: Source BSP is pinned by hash. The [source rows and GPLv3 license](cksurftimer) accompany the derived timer. The map has an independent 3,500-unit per-axis cap and a 350-unit start-speed cap. The original timer plugin applies its start-speed policy differently, so this route is available in Practice under the desktop movement profile.

## Additional Counter-Strike: Source Practice routes

The author-marked timer brushes are read from each pinned map. The two wrldspawn imports use the included [Unlicense](LICENSE). Published KSF replays provide 3,500-unit velocity-cap evidence for Brutalist, Delight, and Rubik's Cube. Other server settings still need verification.

| Map | Tier source | Timer data |
| --- | --- | --- |
| Brutalist | [KSF](https://ksf.surf/maps/surf_brutalist) | Compiled map |
| Chasm | [KSF](https://ksf.surf/maps/surf_chasm) | Compiled map |
| Delight | [KSF](https://ksf.surf/maps/surf_delight) | Compiled map |
| Enlightened | [KSF](https://ksf.surf/maps/surf_enlightened_ksf) | [wrldspawn](https://github.com/wrldspawn/surf-zones/tree/51025635e19b833c00bb69c88559cea087434e0d) |
| Glass 7 | [KSF](https://ksf.surf/maps/surf_glass7_ksf) | Compiled map |
| Lament | [KSF](https://ksf.surf/maps/surf_lament_ksf) | [wrldspawn](https://github.com/wrldspawn/surf-zones/tree/51025635e19b833c00bb69c88559cea087434e0d) |
| Loentra | [KSF](https://ksf.surf/maps/surf_loentra) | Compiled map and authored prestart destination |
| Rubik's Cube | [KSF](https://ksf.surf/maps/surf_rubiks_cube) | Compiled map |
| Void | [KSF](https://ksf.surf/maps/surf_void) | Compiled map and authored jump-suppression triggers |

## Jaunt Bhop Practice routes

Jaunt 3, 4, and 6 by Curly use their original Tier 1 main routes with a 10 ms movement step, air acceleration of 1,000, gravity of 800, a 3,500-unit per-axis velocity cap, and a 290-unit start-speed limit. They use autohop and the floor-relative landing correction. The published route boxes match the pinned BSPs. Eleven published runs reproduced their finish command and displayed time on these settings. [Jaunt 3 records](https://bunnyhop.lol/api/leaderboards/bhop_jaunt_3/autohop?limit=50) · [Jaunt 4 records](https://bunnyhop.lol/api/leaderboards/bhop_jaunt_4/autohop?limit=50) · [Jaunt 6 records](https://bunnyhop.lol/api/leaderboards/bhop_jaunt_6/autohop?limit=50)

Jaunt 6 embeds its pink, black, and cyan surface textures in the BSP. These surfaces display with their original colors on a clean desktop install. [Published map preview](https://bunnyhop.lol/maps/bhop_jaunt_6)

## Counter-Strike: Global Offensive Lavaflow

Lavaflow by Kiiru is a Tier 2 linear course with two checkpoints. The desktop downloads the exact map from the original Workshop listing. Its file size and checksum match the published timer archive. Practice uses air acceleration of 150, ground acceleration of 10, friction of 5.2, a 3,500-unit per-axis cap, and a 260-unit start-speed limit. The historical server tickrate and timer's exact prespeed behavior still need verification. [Workshop release](https://steamcommunity.com/sharedfiles/filedetails/?id=465027021) · [Timer archive](https://github.com/Sayt123/SurfZones/tree/78b5d079f0534eeb464ad1cdf8137a65713522d9)

## Additional Counter-Strike: Global Offensive Practice routes

These Workshop downloads match the BSP sizes and MD5 checksums published by the [SurfZones archive](https://github.com/Sayt123/SurfZones/tree/78b5d079f0534eeb464ad1cdf8137a65713522d9). The client also checks SHA-256 before loading them. Practice uses a 15 ms step, air acceleration 150, ground acceleration 10, friction 5.2, and a 3,500-unit per-axis cap. Exact historical server timing and one-jump prespeed behavior remain unverified.

| Course | Tier | Main checkpoints | Bonuses | Practice start-speed limit |
| --- | ---: | ---: | ---: | ---: |
| [Greensway](https://steamcommunity.com/sharedfiles/filedetails/?id=2129923400) | 2 | 2 | 0 | 350 |
| [Satellite Fix](https://steamcommunity.com/sharedfiles/filedetails/?id=1637463610) | 1 | 2 | 0 | 260 |
| [Tranquil](https://steamcommunity.com/sharedfiles/filedetails/?id=2183939813) | 2 | 3 | 3 | 350 |

Satellite Fix uses the BSP's authored gravity outputs. Tranquil uses the authored bonus spawns and preserves both main finish zones. Each entry has a separate ID from its Counter-Strike: Source counterpart.

[Aweles](https://steamcommunity.com/sharedfiles/filedetails/?id=523350484) by Mazzo is Tier 1 with five stages, four checkpoints, and no bonuses. Its stage starts use the compiled map's original reset destinations. These are above the timer brushes, so the player falls into each start at the original map position and yaw. Practice uses a 260-unit start-speed limit.

Across these three maps, 375 posted SurfHeaven times fall within the display's rounding tolerance of a 64 Hz clock. The records do not identify the BSP by checksum or expose command replays. The 64 Hz inference applies to those boards, and the Practice settings use the native 15 ms clock. [Greensway records](https://csgo.surfheaven.eu/map/surf_greensway) · [Satellite Fix records](https://csgo.surfheaven.eu/map/surf_satellite_fix) · [Tranquil records](https://csgo.surfheaven.eu/map/surf_tranquil)

## Counter-Strike: Global Offensive Forbidden Ways

Krusty's **Forbidden Ways (CS:GO)** is a Tier 1 Practice course with six main checkpoints, two main finish zones, and one bonus. The original Workshop BSP matches the SurfZones archive's size and MD5. Practice uses a 15 ms step, air acceleration of 150, ground acceleration of 10, friction of 5.2, a 3,500-unit per-axis velocity cap, and a 260-unit start-speed limit on both routes. [Workshop release](https://steamcommunity.com/sharedfiles/filedetails/?id=675060874) · [SurfZones timer archive](https://github.com/Sayt123/SurfZones/tree/78b5d079f0534eeb464ad1cdf8137a65713522d9)

Source rules revision 8 applies the archive's Stripper correction to the start booster with Hammer ID 2808. A native movement probe measured 60 units of travel through the corrected booster in one tick, up from 12 units on the unmodified BSP. Both starts reached the grounded, ready state. The historical server tickrate and full-route replay parity remain unverified. [Authored Stripper configuration](https://raw.githubusercontent.com/Sayt123/SurfZones/78b5d079f0534eeb464ad1cdf8137a65713522d9/Stripper%20Files/maps/surf_forbidden_ways_ksf.cfg)

## Counter-Strike: Source Surf course credits

The four wrldspawn timers use the Unlicense course data. The other timers reference each map author's compiled timer brushes directly, with published tiers and named authors from KSF. KSF lists 0 for some speed caps, which indicates an unspecified per-map override. The documented native 5,000-unit per-axis cap applies on those maps. This is the desktop ranked ruleset.

| Map | Creator | Tier | Main checkpoints / stages | Bonuses | Timer source |
| --- | --- | ---: | ---: | ---: | --- |
| surf_004_fix | monkeywithagun | 2 | 2 / 0 | 0 | [wrldspawn](https://github.com/wrldspawn/surf-zones/tree/51025635e19b833c00bb69c88559cea087434e0d) |
| surf_6 | vay | 2 | 2 / 0 | 1 | [wrldspawn](https://github.com/wrldspawn/surf-zones/tree/51025635e19b833c00bb69c88559cea087434e0d) |
| surf_mom_fix | Dizzy | 1 | 5 / 6 | 1 | [wrldspawn](https://github.com/wrldspawn/surf-zones/tree/51025635e19b833c00bb69c88559cea087434e0d) |
| surf_water_run_ksf | booskibro | 1 | 3 / 4 | 1 | [wrldspawn](https://github.com/wrldspawn/surf-zones/tree/51025635e19b833c00bb69c88559cea087434e0d) |
| surf_4head | sjom7e | 2 | 3 / 0 | 4 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_4head) |
| surf_amaro | Flaudgin | 6 | 4 / 0 | 1 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_amaro) |
| surf_aquaflow | NvC_DmN_CH | 2 | 2 / 0 | 0 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_aquaflow) |
| surf_conserve | tflo | 5 | 3 / 0 | 0 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_conserve) |
| surf_koa | TeMP | 4 | 3 / 0 | 1 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_koa) |
| surf_marah | Flaudgin | 6 | 4 / 0 | 0 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_marah) |
| surf_palais | Dizzy | 2 | 3 / 0 | 0 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_palais) |
| surf_reverie | elly | 2 | 3 / 0 | 0 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_reverie) |
| surf_second | not_a_zombie | 3 | 3 / 0 | 1 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_second) |
| surf_sentient | TeMP | 4 | 3 / 0 | 1 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_sentient) |
| surf_shy | greyce | 2 | 2 / 0 | 2 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_shy) |
| surf_zealand | Xbmann & Frosty Scales | 2 | 3 / 0 | 2 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_zealand) |
| surf_zoomathon | sid | 1 | 7 / 0 | 0 | [Compiled map and KSF entry](https://ksf.surf/maps/surf_zoomathon) |

Surf 004 Fix uses the two-checkpoint wrldspawn Shavit layout.
