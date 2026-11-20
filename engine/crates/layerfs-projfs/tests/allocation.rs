//! How much of the volume a projected file holds (report section 7.5).
//!
//! Observed: ProjFS reserves a placeholder's space in units of 16 clusters,
//! as NTFS does for a sparse file, though the file does not show the sparse
//! attribute. A hydrated placeholder holds whole units. Whatever releases
//! the unused part also turns the file into a full file: the provider
//! cannot have a file that is both a placeholder and compact.
#![cfg(windows)]

mod support;

use layerfs_projfs::source::directory::DirectorySource;
use layerfs_projfs::sys;
use layerfs_projfs::Projection;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::sync::Arc;
use support::ops::{self, Op};
use support::Scratch;

#[link(name = "kernel32")]
extern "system" {
    fn GetFileInformationByHandleEx(
        file: *mut std::ffi::c_void,
        class: i32,
        information: *mut std::ffi::c_void,
        size: u32,
    ) -> i32;
    fn DeviceIoControl(
        device: *mut std::ffi::c_void,
        code: u32,
        input: *const std::ffi::c_void,
        input_size: u32,
        output: *mut std::ffi::c_void,
        output_size: u32,
        returned: *mut u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
}

/// `FILE_STANDARD_INFO`.
#[repr(C)]
#[derive(Default)]
struct Standard {
    allocation: i64,
    end: i64,
    links: u32,
    delete_pending: u8,
    directory: u8,
}

const FILE_STANDARD_INFO_CLASS: i32 = 1;
const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;
const FILE_WRITE_ATTRIBUTES: u32 = 0x100;

/// The bytes of the volume the file at `path` holds.
fn allocated(path: &Path) -> u64 {
    let file = std::fs::File::open(path).expect("open");
    let mut info = Standard::default();
    // SAFETY: the handle is open; `info` is writable and its size is passed.
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle().cast(),
            FILE_STANDARD_INFO_CLASS,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<Standard>() as u32,
        )
    };
    assert_ne!(read, 0, "{}", std::io::Error::last_os_error());
    info.allocation as u64
}

/// Asks the file system to clear the sparse flag of `file`.
fn clear_sparse(file: &std::fs::File) -> std::io::Result<()> {
    let (off, mut returned) = (0u8, 0u32);
    // SAFETY: the handle is open; the one-byte input outlives the call.
    let done = unsafe {
        DeviceIoControl(
            file.as_raw_handle().cast(),
            FSCTL_SET_SPARSE,
            std::ptr::from_ref(&off).cast(),
            1,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if done == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn sparse(path: &Path) -> bool {
    let file = std::fs::File::open(path).expect("open");
    sys::identity(&file).expect("identity").attributes & FILE_ATTRIBUTE_SPARSE_FILE != 0
}

fn state(path: &Path) -> i32 {
    sys::disk_state(path).expect("state").expect("on disk")
}

/// Waits, bounded, until the file at `path` is a full file that holds no
/// more of the volume than `plain`. ProjFS finishes turning a file into a
/// full file a moment after the call that caused it returns: looked at at
/// once, the file can still read as a dirty placeholder (seen in about one
/// run in forty of this test, and for most files in a tight loop).
fn settles_full_and_compact(path: &Path, plain: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (now, held, wanted) = (state(path), allocated(path), allocated(plain));
        if now == sys::state::FULL && held == wanted {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{}: state {now}, {held} bytes of the volume against {wanted}",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

struct Mounted {
    home: Scratch,
    source: std::path::PathBuf,
    root: std::path::PathBuf,
    projection: Projection,
    /// The volume's cluster size, from a plain file one byte over 1 MiB.
    cluster: u64,
}

fn mount(label: &str) -> Mounted {
    let home = Scratch::new(label);
    let source = home.path().join("source");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(source.join("small.txt"), support::pattern(9, 5_000)).expect("file");
    std::fs::write(source.join("large.bin"), support::pattern(4, 200_000)).expect("file");
    let ruler = home.path().join("ruler.bin");
    std::fs::write(&ruler, vec![1u8; (1 << 20) + 1]).expect("file");
    let cluster = allocated(&ruler) - (1 << 20);
    let root = home.path().join("root");
    let projection = Projection::mount(
        &root,
        &home.path().join("state"),
        Arc::new(DirectorySource::new(&source)),
        0,
    )
    .expect("mount");
    Mounted {
        home,
        source,
        root,
        projection,
        cluster,
    }
}

#[test]
fn a_hydrated_file_holds_whole_units_of_sixteen_clusters() {
    let mounted = mount("allocation-units");
    let unit = 16 * mounted.cluster;
    ops::run(
        mounted.home.path(),
        &[
            Op::Read(mounted.root.join("small.txt")),
            Op::Read(mounted.root.join("large.bin")),
        ],
    );
    for (name, length) in [("small.txt", 5_000u64), ("large.bin", 200_000)] {
        let (projected, plain) = (mounted.root.join(name), mounted.source.join(name));
        assert_eq!(
            state(&projected),
            sys::state::PLACEHOLDER | sys::state::HYDRATED
        );
        // The same bytes in a plain file round up to a cluster; here they
        // round up to a unit, and the file does not show as sparse.
        assert_eq!(
            allocated(&plain),
            length.div_ceil(mounted.cluster) * mounted.cluster
        );
        assert_eq!(
            allocated(&projected),
            length.div_ceil(unit) * unit,
            "{name}"
        );
        assert!(!sparse(&projected), "{name}");
    }
    mounted.projection.discard().expect("discard");
}

#[test]
fn releasing_the_space_makes_the_file_a_full_file() {
    let mounted = mount("allocation-release");
    let (small, large) = (
        mounted.root.join("small.txt"),
        mounted.root.join("large.bin"),
    );
    ops::run(
        mounted.home.path(),
        &[Op::Read(small.clone()), Op::Read(large.clone())],
    );

    // The provider's own process clears the sparse flag through a handle
    // that can only write attributes. The request succeeds, the space is
    // released, and the file ends up no longer a placeholder.
    {
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .open(&small)
            .expect("open for write-attributes");
        clear_sparse(&file).expect("clear the sparse flag");
    }
    settles_full_and_compact(&small, &mounted.source.join("small.txt"));

    // Another process opening the file for writing, writing nothing, does
    // the same.
    ops::run(
        mounted.home.path(),
        &[Op::Hold(large.clone(), Vec::new(), 0)],
    );
    settles_full_and_compact(&large, &mounted.source.join("large.bin"));

    // Neither file changed: both read as the base and status is empty.
    assert_eq!(
        std::fs::read(&small).expect("read"),
        support::pattern(9, 5_000)
    );
    assert_eq!(
        std::fs::read(&large).expect("read"),
        support::pattern(4, 200_000)
    );
    assert_eq!(mounted.projection.status().expect("status"), Vec::new());
    mounted.projection.discard().expect("discard");
}

#[test]
fn child_process_entry() {
    ops::child_entry();
}
