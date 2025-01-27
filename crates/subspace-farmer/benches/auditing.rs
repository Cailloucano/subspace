use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use futures::executor::block_on;
use memmap2::Mmap;
use std::fs::OpenOptions;
use std::io::Write;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use std::{env, fs, io};
use subspace_archiving::archiver::Archiver;
use subspace_core_primitives::crypto::kzg;
use subspace_core_primitives::crypto::kzg::Kzg;
use subspace_core_primitives::{
    plot_sector_size, Blake2b256Hash, Piece, PublicKey, SolutionRange, PIECES_IN_SEGMENT,
    RECORD_SIZE,
};
use subspace_farmer::file_ext::FileExt;
use subspace_farmer::single_disk_plot::farming::audit_sector;
use subspace_farmer::single_disk_plot::plotting::plot_sector;
use subspace_rpc_primitives::FarmerProtocolInfo;
use utils::BenchPieceReceiver;

mod utils;

// This is helpful for overriding locally for benching different parameters
pub const RECORDED_HISTORY_SEGMENT_SIZE: u32 = RECORD_SIZE * PIECES_IN_SEGMENT / 2;

pub fn criterion_benchmark(c: &mut Criterion) {
    let base_path = env::var("BASE_PATH")
        .map(|base_path| base_path.parse().unwrap())
        .unwrap_or_else(|_error| env::temp_dir());
    let sectors_count = env::var("SECTORS_COUNT")
        .map(|sectors_count| sectors_count.parse().unwrap())
        .unwrap_or(10);

    let public_key = PublicKey::default();
    let sector_index = 0;
    let input = vec![1u8; RECORDED_HISTORY_SEGMENT_SIZE as usize];
    let kzg = Kzg::new(kzg::test_public_parameters());
    let mut archiver = Archiver::new(RECORD_SIZE, RECORDED_HISTORY_SEGMENT_SIZE, kzg).unwrap();
    let piece = Piece::try_from(
        archiver
            .add_block(input, Default::default())
            .into_iter()
            .next()
            .unwrap()
            .pieces
            .as_pieces()
            .next()
            .unwrap(),
    )
    .unwrap();

    let cancelled = AtomicBool::new(false);
    let farmer_protocol_info = FarmerProtocolInfo {
        genesis_hash: Default::default(),
        record_size: NonZeroU32::new(RECORD_SIZE).unwrap(),
        recorded_history_segment_size: RECORDED_HISTORY_SEGMENT_SIZE,
        total_pieces: NonZeroU64::new(1).unwrap(),
        space_l: NonZeroU16::new(20).unwrap(),
        sector_expiration: 1,
    };
    let global_challenge = Blake2b256Hash::default();
    let solution_range = SolutionRange::MAX;

    let plot_sector_size = plot_sector_size(farmer_protocol_info.space_l);

    let plotted_sector = {
        let mut plotted_sector = vec![0u8; plot_sector_size as usize];

        block_on(plot_sector(
            &public_key,
            sector_index,
            &BenchPieceReceiver::new(piece),
            &cancelled,
            &farmer_protocol_info,
            plotted_sector.as_mut_slice(),
            io::sink(),
        ))
        .unwrap();

        plotted_sector
    };

    let mut group = c.benchmark_group("audit");
    group.throughput(Throughput::Elements(1));
    group.bench_function("memory", |b| {
        b.iter(|| {
            audit_sector(
                black_box(&public_key),
                black_box(sector_index),
                black_box(&farmer_protocol_info),
                black_box(&global_challenge),
                black_box(solution_range),
                black_box(io::Cursor::new(&plotted_sector)),
            )
            .unwrap();
        })
    });

    group.throughput(Throughput::Elements(sectors_count));
    group.bench_function("disk", |b| {
        let plot_file_path = base_path.join("subspace_bench_sector.bin");
        let mut plot_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&plot_file_path)
            .unwrap();

        plot_file
            .preallocate(plot_sector_size * sectors_count)
            .unwrap();
        plot_file.advise_random_access().unwrap();

        for _i in 0..sectors_count {
            plot_file.write_all(plotted_sector.as_slice()).unwrap();
        }

        let plot_mmap = unsafe { Mmap::map(&plot_file).unwrap() };

        #[cfg(unix)]
        {
            plot_mmap.advise(memmap2::Advice::Random).unwrap();
        }

        b.iter_custom(|iters| {
            let start = Instant::now();
            for _i in 0..iters {
                for (sector_index, sector) in plot_mmap
                    .chunks_exact(plot_sector_size as usize)
                    .enumerate()
                    .map(|(sector_index, sector)| (sector_index as u64, sector))
                {
                    audit_sector(
                        black_box(&public_key),
                        black_box(sector_index),
                        black_box(&farmer_protocol_info),
                        black_box(&global_challenge),
                        black_box(solution_range),
                        black_box(io::Cursor::new(sector)),
                    )
                    .unwrap();
                }
            }
            start.elapsed()
        });

        drop(plot_file);
        fs::remove_file(plot_file_path).unwrap();
    });
    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
