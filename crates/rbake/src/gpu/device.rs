//! The wgpu compute device: one dispatch that answers the visibility of every
//! lightmap texel, and a readback the caller compares against the CPU.

use wgpu_types::Maintain;

use crate::bvh::Bvh;
use crate::error::{Error, Result};
use crate::gpu::{cpu_visibility, GpuNode, RayBatch, Visibility, VisibilityPage, SHADER};
use crate::light::DirectContext;

const WORKGROUP: u32 = 64;

struct Buffers {
    nodes: wgpu::Buffer,
    tris: wgpu::Buffer,
    origins: wgpu::Buffer,
    visibility: wgpu::Buffer,
    dir: wgpu::Buffer,
    readback: wgpu::Buffer,
}

impl Buffers {
    fn destroy(self) {
        self.nodes.destroy();
        self.tris.destroy();
        self.origins.destroy();
        self.visibility.destroy();
        self.dir.destroy();
        self.readback.destroy();
    }
}

fn storage(device: &wgpu::Device, label: &str, size: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.max(16) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn readback(device: &wgpu::Device, size: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rbake.readback"),
        size: size.max(16) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Runs the compute pass, or reports that no adapter was available.
pub fn run(ctx: &DirectContext, batch: &RayBatch) -> Result<Visibility> {
    if batch.is_empty() {
        return Ok(cpu_visibility(ctx, batch));
    }
    let Some(device) = acquire(ctx.bvh) else {
        return Ok(cpu_visibility(ctx, batch));
    };
    match dispatch(&device, ctx.bvh, batch) {
        Ok(v) => {
            device.release();
            Ok(v)
        }
        Err(e) => {
            device.release();
            Err(e)
        }
    }
}

/// A device plus the queue, taken once and reused.
pub struct Device {
    inner: wgpu::Device,
    queue: wgpu::Queue,
}

impl Device {
    /// Blocks until the queue has drained, then drops the device.
    fn release(self) {
        let _ = self.inner.poll(Maintain::Wait);
    }
}

/// Blocks until the queue has drained.
fn wait(device: &wgpu::Device) -> Result<()> {
    let _ = device.poll(Maintain::Wait);
    Ok(())
}

fn acquire(_bvh: &Bvh) -> Option<Device> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))?;
    let (inner, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("rbake"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .ok()?;
    Some(Device { inner, queue })
}

fn dispatch(device: &Device, bvh: &Bvh, batch: &RayBatch) -> Result<Visibility> {
    let nodes: Vec<GpuNode> = bvh.nodes.iter().map(GpuNode::from).collect();
    if !bytemuck::cast_slice::<GpuNode, u8>(&nodes).len().is_multiple_of(16) {
        return Err(Error::lighting("the node buffer is not a whole number of 16-byte records"));
    }
    let tris: Vec<[f32; 12]> = bvh
        .triangles
        .iter()
        .map(|t| {
            [
                t.a.x, t.a.y, t.a.z, t.b.x, t.b.y, t.b.z, t.c.x, t.c.y, t.c.z, 0.0, 0.0, 0.0,
            ]
        })
        .collect();
    let origins: Vec<[f32; 4]> = batch.origins.iter().map(|o| [o[0], o[1], o[2], 0.0]).collect();
    let mut vis: Vec<f32> = vec![1.0; batch.origins.len()];

    let n_bytes = bytemuck::cast_slice::<GpuNode, u8>(&nodes).len();
    let t_bytes = bytemuck::cast_slice::<[f32; 12], u8>(&tris).len();
    let o_bytes = bytemuck::cast_slice::<[f32; 4], u8>(&origins).len();
    let v_bytes = vis.len() * 4;

    let buffers = Buffers {
        nodes: storage(&device.inner, "rbake.nodes", n_bytes),
        tris: storage(&device.inner, "rbake.tris", t_bytes),
        origins: storage(&device.inner, "rbake.origins", o_bytes),
        visibility: storage(&device.inner, "rbake.visibility", v_bytes),
        dir: device.inner.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rbake.dir"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
        readback: readback(&device.inner, v_bytes),
    };
    device.queue.write_buffer(&buffers.nodes, 0, bytemuck::cast_slice::<GpuNode, u8>(&nodes));
    device
        .queue
        .write_buffer(&buffers.tris, 0, bytemuck::cast_slice::<[f32; 12], u8>(&tris));
    device
        .queue
        .write_buffer(&buffers.origins, 0, bytemuck::cast_slice::<[f32; 4], u8>(&origins));
    let mut dir = [0.0f32; 4];
    dir[..3].copy_from_slice(&batch.direction);
    device.queue.write_buffer(&buffers.dir, 0, bytemuck::cast_slice::<f32, u8>(&dir));

    let module = device.inner.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("rbake.traverse"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let buffer = |index: u32, read_only: bool| {
        wgpu::BindGroupLayoutEntry {
            binding: index,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }
    };
    let uniform = wgpu::BindGroupLayoutEntry {
        binding: 4,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let layout = device.inner.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("rbake.layout"),
        entries: &[buffer(0, true), buffer(1, true), buffer(2, true), buffer(3, false), uniform],
    });
    let pipeline_layout = device.inner.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("rbake.pipeline"),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.inner.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("rbake.traverse"),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let bind = device.inner.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rbake.bind"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: buffers.nodes.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: buffers.tris.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: buffers.origins.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: buffers.visibility.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 4, resource: buffers.dir.as_entire_binding() },
        ],
    });

    let groups = (vis.len() as u32).div_ceil(WORKGROUP);
    let mut enc = device
        .inner
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rbake") });
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rbake.pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(groups.max(1), 1, 1);
    }
    enc.copy_buffer_to_buffer(&buffers.visibility, 0, &buffers.readback, 0, v_bytes as u64);
    device.queue.submit(Some(enc.finish()));
    wait(&device.inner)?;
    {
        let slice = buffers.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        wait(&device.inner)?;
        rx
            .recv()
            .map_err(|_| Error::lighting("the compute readback channel closed"))?
            .map_err(|e| Error::lighting(format!("the compute readback did not map: {e}")))?;
        let view = slice.get_mapped_range();
        let bytes = view.as_ref();
        let n = (bytes.len() / 4).min(vis.len());
        for (i, slot) in vis.iter_mut().enumerate().take(n) {
            *slot = f32::from_le_bytes([bytes[i * 4], bytes[i * 4 + 1], bytes[i * 4 + 2], bytes[i * 4 + 3]]);
        }
        drop(view);
        buffers.readback.unmap();
    }
    buffers.destroy();

    let mut pages: Vec<VisibilityPage> = batch
        .pages
        .iter()
        .map(|p| VisibilityPage {
            width: p.width,
            height: p.height,
            values: vec![1.0; (p.width * p.height) as usize],
        })
        .collect();
    for (i, t) in batch.targets.iter().enumerate() {
        let Some(v) = vis.get(i) else { continue };
        if let Some(p) = pages.get_mut(t[0] as usize) {
            if let Some(slot) = p.values.get_mut(t[1] as usize) {
                *slot = *v;
            }
        }
    }
    Ok(Visibility { pages })
}
