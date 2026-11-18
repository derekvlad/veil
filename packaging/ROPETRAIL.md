# ropetrail particle patch

Wallpaper Engine scenes that use the `ropetrail` particle renderer (shooting
stars, comet tails) rendered as one continuous ribbon threaded through every
particle of the emitter instead of a separate trail behind each particle. On
a scene with 500 stars that produces long straight lines across the screen.

Cause: `CParticle` treats `rope` and `ropetrail` the same way. Both set
`m_useRopeRenderer`, and `renderRope()` joins `m_particles[0..count-1]` into a
single Catmull-Rom spline. The `segments` field from the particle JSON (how
many history snapshots belong to one particle) was parsed into `m_ropeSegments`
but never read, which is the giveaway that the trail case was never
implemented.

Upstream reports: Almamu/linux-wallpaperengine#682 (rope trails freeze and
accumulate), #410 (particle sizes). A full particle rewrite lives in the
sgtaziz fork; this patch is the minimal fix for `ropetrail` alone.

## What the patch changes

- `CParticle` keeps a history row of `m_ropeSegments` positions per particle,
  so compacting dead particles cannot splice two unrelated trails together.
- A new particle resets its row to its spawn point, so it draws a short streak
  that grows, not a line reaching back to whatever used the slot before.
- History is sampled on a fixed interval (`length / segments`), matching how
  Wallpaper Engine times it.
- `renderRopeTrail()` builds one ribbon per live particle from its head plus
  its own samples, with the tail fading out. Plain `rope` is untouched.
- `g_RenderVar0` carries the sampling phase and sample budget the
  `genericropeparticle` shader expects for trails.

Files: `src/WallpaperEngine/Render/Objects/CParticle.{cpp,h}`.

## Applying

    cd linux-wallpaperengine && patch -p1 < linux-wallpaperengine-ropetrail.patch
    cmake -B build -S . -DCMAKE_BUILD_TYPE=Release \
          -DCMAKE_INSTALL_PREFIX=/opt/linux-wallpaperengine
    cmake --build build -j$(nproc)

Built and checked against r643.a0ddce58. Verify on a ropetrail scene such as
`3299228616` (Lonely Cat) or `2915841260` (Sonic Frontiers Starfall): streaks
should be short and move with their star, and `spritetrail` scenes (`1260343035`
snow) must look unchanged.
