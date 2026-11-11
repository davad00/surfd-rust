use bevy::{asset::AssetId, prelude::*};

pub struct UiFontPlugin;

impl Plugin for UiFontPlugin {
    fn build(&self, app: &mut App) {
        // Full Fira Mono from mozilla/Fira. The bundled license accompanies the face.
        // Keep this at the default asset ID so all existing UI and HUD text uses it.
        app.world_mut()
            .resource_mut::<Assets<Font>>()
            .insert(
                AssetId::default(),
                Font::from_bytes(include_bytes!("../assets/fonts/FiraMono-Regular.ttf").to_vec()),
            )
            .expect("the default font asset exists after DefaultPlugins");
    }
}
