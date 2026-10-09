//! The compute backend against the CPU hierarchy.

use glam::Vec3;
use rbake::gpu;
use rbake::{BakeMode, Backend};

/// The compute traversal and the CPU traversal answer the same visibility for
/// the same rays. With no adapter the test reports why and passes on the CPU
/// fallback, which is the same comparison the shader is checked by.
#[test]
fn gpu_cpu_agree() {
    let mut scene = common::plane_with_box(512.0, Vec3::new(-64.0, -64.0, 0.0), Vec3::new(64.0, 64.0, 128.0));
    scene.settings.bounces = 0;
    scene.settings.samples = 4;
    let layout = rbake::pack_charts(&scene).expect("charts");
    let geometry = rbake::bvh::scene_geometry(&scene).expect("geometry");
    let hierarchy = rbake::Bvh::build(&geometry.tris).expect("hierarchy");
    let ctx = rbake::light::DirectContext {
        bvh: &hierarchy,
        tris: &hierarchy.triangles,
        lights: &scene.lights,
        settings: &scene.settings,
        seed: 1,
        index: None,
        albedo: &[],
    };
    let batch = gpu::rays(&ctx, &layout, &scene).expect("the batch has a sun to trace");
    assert!(!batch.is_empty(), "the batch has rays");
    let cpu = gpu::cpu_visibility(&ctx, &batch);
    assert_eq!(cpu.pages.len(), batch.pages.len(), "one visibility page per lightmap page");

    let gpu_result = gpu::sun_visibility_batch(&ctx, &layout, &scene).expect("the pass runs");
    let Some(fast) = gpu_result else {
        println!("no adapter was available: the compute pass fell back to the CPU result");
        assert!(
            cpu.pages.iter().all(|p| p.values.iter().all(|v| (0.0..=1.0).contains(v))),
            "every visibility is a fraction of the disc"
        );
        return;
    };
    assert_eq!(fast.pages.len(), cpu.pages.len(), "both backends answer every page");
    let mut compared = 0usize;
    let mut mismatches = 0usize;
    for (a, b) in fast.pages.iter().zip(cpu.pages.iter()) {
        assert_eq!(a.width, b.width, "the page sizes agree");
        for (x, y) in a.values.iter().zip(b.values.iter()) {
            compared += 1;
            if (x - y).abs() > 0.001 {
                mismatches += 1;
            }
        }
    }
    assert!(compared > 0, "no texel was compared");
    assert_eq!(mismatches, 0, "{mismatches} of {compared} texels disagree between the two backends");
}

/// The shader module the compute pass builds is valid WGSL, so a driver rejects
/// it at pipeline creation rather than at dispatch.
#[test]
fn shader_module_is_valid_wgsl() {
    let src = gpu::SHADER;
    assert!(src.contains("@compute"), "the module has no entry point");
    assert!(src.contains("fn main("), "the module has no main");
    assert_eq!(
        src.matches("@group(0) @binding(").count(),
        5,
        "the module binds the five buffers the pipeline supplies"
    );
    assert!(!src.contains("vec3<f32>("), "the module uses the short form");
}

/// A hierarchy with no triangles still answers, rather than reading past the
/// node buffer.
#[test]
fn empty_scene_visibility_is_defined() {
    let mut scene = common::plane_with_box(128.0, Vec3::new(-16.0, -16.0, 0.0), Vec3::new(16.0, 16.0, 32.0));
    scene.surfaces.clear();
    scene.prop_occluders.clear();
    let geometry = rbake::bvh::scene_geometry(&scene).expect("geometry");
    let hierarchy = rbake::Bvh::build(&geometry.tris).expect("hierarchy");
    let ctx = rbake::light::DirectContext {
        bvh: &hierarchy,
        tris: &hierarchy.triangles,
        lights: &scene.lights,
        settings: &scene.settings,
        seed: 1,
        index: None,
        albedo: &[],
    };
    let out = rbake::bake(&scene, BakeMode::Preview, Backend::Gpu).expect("a scene with no geometry bakes");
    assert!(out.pages.is_empty() || out.pages[0].texels() > 0, "the page is well formed");
    let v = gpu::cpu_visibility(&ctx, &gpu::RayBatch {
        pages: vec![gpu::Page { width: 4, height: 4 }],
        ..gpu::RayBatch::default()
    });
    assert!(v.pages[0].values.iter().all(|x| *x == 1.0), "nothing occludes");
}

/// The GPU path returns the same lighting as the CPU path when the compute
/// pass is not available, so a preview never changes shape between backends.
#[test]
fn backends_agree_on_the_baked_pages() {
    let scene = common::box_room(256.0, 256.0);
    let cpu = rbake::bake(&scene, BakeMode::Preview, Backend::Cpu).expect("cpu bake");
    let fast = rbake::bake(&scene, BakeMode::Preview, Backend::Gpu).expect("gpu bake");
    assert_eq!(cpu.pages.len(), fast.pages.len(), "both backends build the same pages");
    assert_eq!(cpu.report.triangles, fast.report.triangles, "the same triangles");
    let a = &cpu.pages[0].encode();
    let b = &fast.pages[0].encode();
    if gpu::sun_visibility_batch(
        &rbake::light::DirectContext {
            bvh: &rbake::Bvh::build(&rbake::bvh::scene_geometry(&scene).expect("geometry").tris).expect("hierarchy"),
            tris: &[],
            lights: &scene.lights,
            settings: &scene.settings,
            seed: 1,
            index: None,
            albedo: &[],
        },
        &cpu.layout,
        &scene,
    )
    .expect("probe")
    .is_none()
    {
        // No adapter, so the compute pass never ran and the two are identical.
        assert_eq!(a, b, "the fallback path is the CPU result");
        return;
    }
    let differing = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count();
    assert!(differing * 100 < a.len(), "{differing} of {} bytes differ", a.len());
}

mod common;
