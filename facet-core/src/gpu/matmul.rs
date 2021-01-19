//! compute shaders
//!
#[cfg(test)]
mod tests;

use super::{GpuNdArrayError, EXECUTOR};

use rayon::prelude::*;
use std::sync::Arc;
use vulkano::{
    buffer::{BufferUsage, CpuAccessibleBuffer},
    descriptor::{pipeline_layout::PipelineLayout, PipelineLayoutAbstract},
    device::Device,
    sync::GpuFuture,
};

pub const LOCAL_SIZE_X: u32 = 32;
pub const LOCAL_SIZE_Y: u32 = 16;
/// Number of rows in the left matrix to process at a time
pub const ROW_SPLIT_THRESHOLD: u32 = 512;

/// naive impl, assuming large M and N and small K
// TODO optimize
// maybe something like this? https://www.ibiblio.org/e-notes/webgl/gpu/mul/sgem6.htm
mod gepp {
    vulkano_shaders::shader! {
        ty: "compute",
        src: r#"
#version 450

layout(local_size_x = 32, local_size_y = 16, local_size_z = 1) in;

layout(set = 0, binding = 0) readonly  buffer Data_a { float A[]; };
layout(set = 0, binding = 1) readonly  buffer Data_b { float B[]; };
layout(set = 0, binding = 2) writeonly buffer Data_c { float C[]; };

layout(push_constant) uniform Shape {
    uint M;
    uint K;
    uint N;
};

void main()
{
    uint i = gl_GlobalInvocationID.x; // [0..M)
    uint j = gl_GlobalInvocationID.y; // [0..N)

    float value = 0.0;
    for(uint k = 0; k < K; k++)
    {
        float a = A[i * K + k];
        float b = B[k * N + j];
        value += a * b;
    }

    C[i * N + j] = value;
}"#
    }
}

lazy_static::lazy_static! {
    pub static ref GEPP_PIPE: Arc<vulkano::pipeline::ComputePipeline<PipelineLayout<gepp::Layout>>> = {
        let exc = EXECUTOR.as_ref().unwrap();
        let device = exc.device.clone();
        let shader = match MATMUL.as_ref() {
            Some(shader) => shader,
            None => panic!("{}",GpuNdArrayError::NoShader),
        };
        Arc::new(
            vulkano::pipeline::ComputePipeline::new(
                device.clone(),
                &shader.main_entry_point(),
                &(),
                None,
            )
            .expect("failed to create compute pipeline"),
        )
    };
    pub static ref MATMUL: Option<gepp::Shader> = {
        EXECUTOR.as_ref().and_then(|exc|{ gepp::Shader::load(exc.device.clone()).ok() })
    };
}

pub fn matmul_f32_impl<'a>(
    [m, k, n]: [u32; 3],
    in0: &'a [f32],
    in1: &'a [f32],
    out: &mut [f32],
) -> Result<(), GpuNdArrayError> {
    assert!(m as usize * k as usize <= in0.len());
    assert!(n as usize * k as usize <= in1.len());
    assert!(m as usize * n as usize <= out.len());

    let exc = match EXECUTOR.as_ref() {
        Some(e) => e,
        None => return Err(GpuNdArrayError::NoExecutor),
    };
    let device = exc.device.clone();
    let compute_pipeline = GEPP_PIPE.clone();

    let res = if m > ROW_SPLIT_THRESHOLD {
        // iterate over some of the rows at a time
        let device = device.clone();
        out.par_chunks_mut(n as usize * ROW_SPLIT_THRESHOLD as usize)
            .enumerate()
            .try_for_each(move |(subi, submatrix)| {
                let offset = subi * ROW_SPLIT_THRESHOLD as usize;
                let m = submatrix.len() / n as usize; // 1..ROW_SPLIT
                debug_assert!(m >= 1);
                debug_assert!(in0[offset * k as usize..].len() >= m * k as usize);
                gepp(
                    exc,
                    device.clone(),
                    compute_pipeline.clone(),
                    [m as u32, k, n],
                    &in0[offset * k as usize..],
                    in1,
                    submatrix,
                )
            })
    } else {
        gepp(
            exc,
            device.clone(),
            compute_pipeline,
            [m, k, n],
            in0,
            in1,
            out,
        )
    };

    // we should periodically clean up the gpu resources, for now let's do that in every call
    let mut finish = vulkano::sync::now(device);
    finish.cleanup_finished();

    res
}

/// Assumes large `m` and `n` and small `k`
///
/// multiplies m*k and k*n matrices, output m*n matrix
fn gepp<'a>(
    exc: &super::GpuExecutor,
    device: Arc<Device>,
    compute_pipeline: Arc<vulkano::pipeline::ComputePipeline<PipelineLayout<gepp::Layout>>>,
    // matmul params
    [m, k, n]: [u32; 3],
    in0: &'a [f32],
    in1: &'a [f32],
    out: &mut [f32],
) -> Result<(), GpuNdArrayError> {
    let shape = [m, k, n];

    let ((a_buffer, b_buffer), c_buffer) = rayon::join(
        || {
            rayon::join(
                || matrix_buffer(device.clone(), false, in0.iter().cloned()),
                || matrix_buffer(device.clone(), false, in1.iter().cloned()),
            )
        },
        || matrix_buffer(device.clone(), true, (0..out.len()).map(|_| 0.0f32)),
    );

    // Descriptor sets
    let descriptor = vulkano::descriptor::descriptor_set::PersistentDescriptorSet::start(
        compute_pipeline
            .layout()
            .descriptor_set_layout(0)
            .unwrap()
            .clone(),
    )
    .add_buffer(a_buffer)
    .expect("a buffer")
    .add_buffer(b_buffer)
    .expect("b buffer")
    .add_buffer(c_buffer.clone())
    .expect("c buffer")
    .build()
    .unwrap();

    // Dispatch
    let mut builder =
        vulkano::command_buffer::AutoCommandBufferBuilder::new(device.clone(), exc.queue.family())
            .unwrap();
    let workgroups = [m / LOCAL_SIZE_X, n / LOCAL_SIZE_Y, 1];
    builder
        .dispatch(workgroups, compute_pipeline, descriptor, shape)
        .unwrap();
    let comand_buffer = builder.build().unwrap();

    let finish = vulkano::sync::now(device)
        .then_execute(exc.queue.clone(), comand_buffer)
        .unwrap()
        .then_signal_fence_and_flush()
        .expect("failed to flush");

    // buffers can be reused, ensure 0 initial output value
    out.par_iter_mut().for_each(|x| *x = 0.0);

    // process the remaning columns on the cpu while we await the gpu execution
    // note that the last block is calculated twice, the auther deems this ok for now

    // last columns
    let remaining_n = m % LOCAL_SIZE_X;
    let offset_n = (m - remaining_n) as usize;
    (0..n).for_each(|j| {
        let j = j as usize;
        for i in 0..remaining_n {
            let i = i as usize + offset_n;
            let mut val = 0.0;
            for l in 0..k {
                let l = l as usize;
                val += at(in0, i, k as usize, l) * at(in1, l, n as usize, j);
            }
            out[i * n as usize + j] = val;
        }
    });
    // last rows
    let remaining_p = n % LOCAL_SIZE_Y;
    let offset_p = (n - remaining_p) as usize;
    (0..m).for_each(|i| {
        let i = i as usize;
        for j in 0..remaining_p {
            let j = j as usize + offset_p;
            let mut val = 0.0;
            for l in 0..k {
                let l = l as usize;
                val += at(in0, i, k as usize, l) * at(in1, l, n as usize, j);
            }
            out[i * n as usize + j] = val;
        }
    });

    finish.wait(None).expect("compute shader execution failed");

    // gpu finished processing, copy the result
    let content = c_buffer.read().unwrap();
    out.par_chunks_mut(n as usize)
        .enumerate()
        // only set the out values we haven't touched in the cpu-computations
        .for_each(|(i, b)| {
            let offset = i * n as usize;
            for (i, v) in b.iter_mut().enumerate() {
                // use add as we might touch values skipped by the gpu
                // these values will be set to 0.
                *v += content[offset + i];
            }
        });

    Ok(())
}

#[inline(always)]
fn at(values: &[f32], i: usize, cols: usize, j: usize) -> f32 {
    unsafe { *values.as_ptr().add(i * cols + j) }
}

#[inline]
fn matrix_buffer(
    device: Arc<Device>,
    host_cached: bool,
    data: impl Iterator<Item = f32> + std::iter::ExactSizeIterator,
) -> Arc<CpuAccessibleBuffer<[f32]>> {
    // use CPU accessible buffers (shared buffers) because I found that dedicated GPU memory runs
    // out fast
    CpuAccessibleBuffer::from_iter(device, BufferUsage::all(), host_cached, data).unwrap()
}
