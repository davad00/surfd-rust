use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, OnceLock},
};

use bevy_math::DVec3;

use super::{Brush, Plane, source_dot};

pub(crate) type PrecisePlane = (DVec3, f64);

pub(crate) struct AuthoredZoneGeometry {
    pub min: DVec3,
    pub max: DVec3,
    pub planes: Vec<PrecisePlane>,
}

#[derive(Clone)]
struct AuthoredZonePlanes {
    rounded: Vec<Plane>,
    geometry: Arc<AuthoredZoneGeometry>,
}

/// A brush's planes, generated on first contact for displacement triangles.
#[derive(Clone)]
pub struct BrushPlanes(PlaneStorage);

#[derive(Clone)]
enum PlaneStorage {
    Convex(Vec<Plane>),
    AuthoredZone(Box<AuthoredZonePlanes>),
    Displacement {
        hulls: Arc<Vec<DisplacementHull>>,
        index: usize,
    },
}

impl BrushPlanes {
    pub(crate) fn with_zone_geometry(self, geometry: AuthoredZoneGeometry) -> Self {
        let PlaneStorage::Convex(rounded) = self.0 else {
            return self;
        };
        Self(PlaneStorage::AuthoredZone(Box::new(AuthoredZonePlanes {
            rounded,
            geometry: Arc::new(geometry),
        })))
    }

    pub(crate) fn zone_geometry(&self) -> Option<&AuthoredZoneGeometry> {
        match &self.0 {
            PlaneStorage::AuthoredZone(zone) => Some(&zone.geometry),
            _ => None,
        }
    }

    pub(crate) fn displacement_planes(&self) -> Option<&[PrecisePlane]> {
        match &self.0 {
            PlaneStorage::Displacement { hulls, index } => Some(hulls[*index].precise_planes()),
            _ => None,
        }
    }

    pub(crate) fn displacement_id(&self) -> Option<(u32, u32)> {
        match &self.0 {
            PlaneStorage::Displacement { hulls, index } => hulls[*index].id,
            _ => None,
        }
    }
}

impl From<Vec<Plane>> for BrushPlanes {
    fn from(planes: Vec<Plane>) -> Self {
        Self(PlaneStorage::Convex(planes))
    }
}

impl FromIterator<Plane> for BrushPlanes {
    fn from_iter<T: IntoIterator<Item = Plane>>(planes: T) -> Self {
        Self::from(planes.into_iter().collect::<Vec<_>>())
    }
}

impl Deref for BrushPlanes {
    type Target = Vec<Plane>;

    fn deref(&self) -> &Self::Target {
        match &self.0 {
            PlaneStorage::Convex(planes) => planes,
            PlaneStorage::AuthoredZone(zone) => &zone.rounded,
            PlaneStorage::Displacement { hulls, index } => hulls[*index].rounded_planes(),
        }
    }
}

impl DerefMut for BrushPlanes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        if !matches!(self.0, PlaneStorage::Convex(_)) {
            self.0 = PlaneStorage::Convex(self.to_vec());
        }
        let PlaneStorage::Convex(planes) = &mut self.0 else {
            unreachable!();
        };
        planes
    }
}

impl<'a> IntoIterator for &'a BrushPlanes {
    type Item = &'a Plane;
    type IntoIter = std::slice::Iter<'a, Plane>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub(crate) struct DisplacementHull {
    pub(crate) vertices: [DVec3; 3],
    id: Option<(u32, u32)>,
    precise: OnceLock<Box<[PrecisePlane]>>,
    rounded: OnceLock<Vec<Plane>>,
}

impl DisplacementHull {
    pub(crate) fn new(vertices: [DVec3; 3], id: Option<(u32, u32)>) -> Option<Self> {
        let [a, b, c] = vertices;
        (b - a).cross(c - a).try_normalize()?;
        Some(Self {
            vertices,
            id,
            precise: OnceLock::new(),
            rounded: OnceLock::new(),
        })
    }

    pub(crate) fn brush(hulls: &Arc<Vec<Self>>, index: usize) -> Brush {
        let [a, b, c] = hulls[index].vertices;
        Brush {
            planes: BrushPlanes(PlaneStorage::Displacement {
                hulls: Arc::clone(hulls),
                index,
            }),
            min: a.min(b).min(c).as_vec3(),
            max: a.max(b).max(c).as_vec3(),
        }
    }

    fn precise_planes(&self) -> &[PrecisePlane] {
        self.precise.get_or_init(|| {
            let [a, b, c] = self.vertices;
            let normal = (b - a)
                .cross(c - a)
                .try_normalize()
                .expect("displacement normal was validated at import");
            let axes = [DVec3::X, DVec3::NEG_Z, DVec3::Y];
            let edges = [b - a, c - b, a - c];
            let candidates = [normal].into_iter().chain(axes).chain(
                edges
                    .into_iter()
                    .flat_map(|edge| axes.map(|axis| edge.cross(axis))),
            );
            let mut planes = Vec::new();
            for normal in candidates
                .filter(|axis| axis.length_squared() >= 1e-16)
                .filter_map(DVec3::try_normalize)
            {
                for normal in [normal, -normal] {
                    if planes
                        .iter()
                        .any(|(other, _)| source_dot(*other, normal) > 1.0 - 1e-9)
                    {
                        continue;
                    }
                    let distance = self
                        .vertices
                        .into_iter()
                        .map(|vertex| source_dot(normal, vertex))
                        .fold(f64::NEG_INFINITY, f64::max);
                    planes.push((normal, distance));
                }
            }
            planes.into_boxed_slice()
        })
    }

    fn rounded_planes(&self) -> &Vec<Plane> {
        self.rounded.get_or_init(|| {
            self.precise_planes()
                .iter()
                .map(|(normal, distance)| Plane {
                    normal: normal.as_vec3(),
                    distance: *distance as f32,
                })
                .collect()
        })
    }
}
