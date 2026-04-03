use std::io::Result;

fn main() -> Result<()> {
    prost_build::compile_protos(
        &[
            "proto/vision/ssl_vision_wrapper.proto",
            "proto/vision/ssl_vision_detection.proto",
            "proto/vision/ssl_vision_geometry.proto",
            "proto/gc/ssl_gc_referee_message.proto",
            "proto/gc/ssl_gc_game_event.proto",
            "proto/gc/ssl_gc_common.proto",
            "proto/gc/ssl_gc_geometry.proto",
            "proto/tracked/ssl_vision_detection_tracked.proto",
            "proto/tracked/ssl_vision_wrapper_tracked.proto",
        ],
        &["proto"],
    )?;
    Ok(())
}
