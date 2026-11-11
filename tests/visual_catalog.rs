use std::path::PathBuf;

use bevy_surf::{catalog, surfd_map};

#[test]
#[ignore = "requires one original visual manifest per catalog map"]
fn original_manifests_resolve_native_pack_dependencies() {
    let directory = PathBuf::from(
        std::env::var_os("SURFD_VISUAL_MANIFESTS").expect("Set SURFD_VISUAL_MANIFESTS"),
    );
    let mut checked = 0;
    let mut problems = Vec::new();
    for map in catalog::entries().iter().filter(|map| map.visual.is_some()) {
        let path = directory.join(format!("{}.gz", map.slug));
        match surfd_map::visual_dependencies(&path) {
            Ok(packs) if !packs.is_empty() => {}
            Ok(_) => problems.push(format!("{} has no usable visual packs", map.slug)),
            Err(error) => problems.push(format!("{}: {error}", map.slug)),
        }
        match surfd_map::visual_dependencies_for_gpu(&path, true) {
            Ok(packs) if !packs.is_empty() => {}
            Ok(_) => problems.push(format!("{} has no GPU visual packs", map.slug)),
            Err(error) => problems.push(format!("{} GPU packs: {error}", map.slug)),
        }
        checked += 1;
    }
    println!("Checked {checked} original visual manifests");
    assert!(checked > 0);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
