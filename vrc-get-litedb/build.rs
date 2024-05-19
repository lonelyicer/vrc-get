mod build_target_info;

use crate::build_target_info::*;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=dotnet/vrc-get-litedb.csproj");
    println!("cargo:rerun-if-changed=dotnet/src");
    println!("cargo:rerun-if-changed=dotnet/LiteDB/LiteDB");

    // Note for users of this library:
    // The NativeAOT does not support start-stop-gc so you have to disable it.
    if std::env::var("TARGET").unwrap().contains("linux") {
        // start stop gc is not supported by dotnet.
        println!("cargo:rustc-link-arg=-Wl,-z,nostart-stop-gc");
    }

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let target_info = TargetInformation::from_triple(std::env::var("TARGET").unwrap().as_str());
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());

    let dotnet_out_folder = build_dotnet(&out_dir, &manifest_dir, &target_info);

    let dotnet_built = dotnet_out_folder.join(target_info.output_file_name);
    let dotnet_sdk_folder = dotnet_out_folder.join("sdk");
    let dotnet_framework_folder = dotnet_out_folder.join("framework");

    let patched_lib_folder = out_dir.join("patched-lib");
    std::fs::create_dir_all(&patched_lib_folder).expect("creating patched folder");

    println!(
        "cargo:rustc-link-search=native={path}",
        path = patched_lib_folder.display()
    );
    println!(
        "cargo:rustc-link-search=native={path}",
        path = dotnet_sdk_folder.display()
    );
    println!(
        "cargo:rustc-link-search=native={path}",
        path = dotnet_framework_folder.display()
    );
    println!(
        "cargo:rustc-link-search=native={path}",
        path = dotnet_built.parent().unwrap().display()
    );

    // link bootstrapper
    let bootstrapper = dotnet_sdk_folder.join(target_info.bootstrapper);
    if target_info.family == TargetFamily::Linux || target_info.family == TargetFamily::MacOS {
        // for unix-like platforms, generate a static library from bootstrapperdll and link it
        create_libbootstrapperdll_a(&bootstrapper, &patched_lib_folder, &target_info);
        println!("cargo:rustc-link-lib=static:+whole-archive=bootstrapperdll");
    } else {
        // for windows, generate a .lib file from bootstrapperdll.obj and link it
        create_libbootstrapperdll_lib(&bootstrapper, &patched_lib_folder, &target_info);
        println!("cargo:rustc-link-lib=static:+whole-archive=bootstrapperdll");
    }

    // link prebuilt dotnet
    if target_info.family == TargetFamily::MacOS {
        // for apple platform, we need to fix object file a little
        // see https://github.com/dotnet/runtime/issues/96663

        let patched = patched_lib_folder.join("vrc-get-litedb-patched.a");
        patch_mach_o_from_archive(&dotnet_built, &patched);
        println!("cargo:rustc-link-lib=static:+verbatim=vrc-get-litedb-patched.a");
    } else {
        println!(
            "cargo:rustc-link-lib=static:+verbatim={}",
            dotnet_built.file_name().unwrap().to_string_lossy()
        );
    }

    if target_info.remove_libunwind {
        // for linux musl, duplicated linking libunwind causes linkage error so
        // strip from Runtime.WorkstationGC.a
        let lib_name = "libRuntime.WorkstationGC.a";
        let before = dotnet_sdk_folder.join(lib_name);
        let patched = patched_lib_folder.join(lib_name);
        remove_libunwind(&before, &patched);
    }

    let common_libs: &[&str] = &[
        //"static=Runtime.ServerGC",
        "static=Runtime.WorkstationGC",
        "static=eventpipe-disabled",
    ];

    for lib in common_libs {
        println!("cargo:rustc-link-lib={lib}");
    }

    for lib in target_info.link_libraries {
        println!("cargo:rustc-link-lib={lib}");
    }
}

fn build_dotnet(out_dir: &Path, manifest_dir: &Path, target: &TargetInformation) -> PathBuf {
    let mut command = Command::new("dotnet");
    command.arg("publish");
    command.arg(manifest_dir.join("dotnet/vrc-get-litedb.csproj"));

    // set output paths
    let output_dir = out_dir.join("dotnet").join("lib/");
    command.arg("--output").arg(&output_dir);

    let mut building = OsString::from("-p:VrcGetOutDir=");
    building.push(out_dir.join("dotnet"));
    command.arg(building);

    command.arg("--runtime").arg(target.dotnet_runtime_id);

    if target.patch_mach_o {
        // according to filipnavara, setting S_ATTR_NO_DEAD_STRIP for hydrated section is invalid
        // so use IlcDehydrate=false instead
        command.arg("-p:IlcDehydrate=false");
    }

    let status = command.status().unwrap();
    if !status.success() {
        panic!("failed to build dotnet library");
    }

    output_dir
}

fn patch_mach_o_from_archive(archive: &Path, patched: &Path) {
    let file = std::fs::File::open(archive).expect("failed to open built library");
    let mut archive = ar::Archive::new(std::io::BufReader::new(file));

    let file = std::fs::File::create(patched).expect("failed to create patched library");
    let mut builder = ar::Builder::new(std::io::BufWriter::new(file));

    while let Some(entry) = archive.next_entry() {
        let mut entry = entry.expect("reading library");
        if entry.header().identifier().ends_with(b".o") {
            let mut buffer = vec![0u8; 0];

            std::io::copy(&mut entry, &mut buffer).expect("reading library");

            use object::endian::*;
            use object::from_bytes;
            use object::macho::*;

            let (magic, _) = from_bytes::<U32<BigEndian>>(&buffer).unwrap();
            if magic.get(BigEndian) == MH_MAGIC_64 {
                patch_mach_o_64(&mut buffer, Endianness::Big);
            } else if magic.get(BigEndian) == MH_CIGAM_64 {
                patch_mach_o_64(&mut buffer, Endianness::Little);
            } else {
                panic!("invalid mach-o: unknown magic");
            }

            builder
                .append(entry.header(), std::io::Cursor::new(buffer))
                .expect("copying file in archive");
        } else {
            builder
                .append(&entry.header().clone(), &mut entry)
                .expect("copying file in archive");
        }
    }

    builder
        .into_inner()
        .unwrap()
        .flush()
        .expect("writing patched library");

    Command::new("ranlib")
        .arg(patched)
        .status()
        .expect("running ranlib");
}

fn patch_mach_o_64<E: object::Endian>(as_slice: &mut [u8], endian: E) {
    use object::macho::*;
    use object::{from_bytes_mut, slice_from_bytes_mut};

    let (header, as_slice) = from_bytes_mut::<MachHeader64<E>>(as_slice).unwrap();
    let command_count = header.ncmds.get(endian);
    let mut as_slice = as_slice;
    for _ in 0..command_count {
        let (cmd, _) = from_bytes_mut::<LoadCommand<E>>(as_slice).unwrap();
        let cmd_size = cmd.cmdsize.get(endian) as usize;
        if cmd.cmd.get(endian) == LC_SEGMENT_64 {
            let data = &mut as_slice[..cmd_size];
            let (cmd, data) = from_bytes_mut::<SegmentCommand64<E>>(data).unwrap();
            let section_count = cmd.nsects.get(endian);
            let (section_headers, _) =
                slice_from_bytes_mut::<Section64<E>>(data, section_count as usize).unwrap();
            for section_header in section_headers {
                if should_not_dead_strip(section_header, endian) {
                    // __modules section in the data segment
                    let flags = section_header.flags.get(endian);
                    let flags = flags | S_ATTR_NO_DEAD_STRIP;
                    section_header.flags.set(endian, flags);
                }
            }
        }
        as_slice = &mut as_slice[cmd_size..];
    }

    fn should_not_dead_strip<E: object::Endian>(section_header: &Section64<E>, endian: E) -> bool {
        if section_header.flags.get(endian) & S_ZEROFILL != 0 {
            return false;
        }

        if &section_header.segname == b"__DATA\0\0\0\0\0\0\0\0\0\0" {
            return true;
        }

        if &section_header.segname == b"__TEXT\0\0\0\0\0\0\0\0\0\0"
            && &section_header.sectname == b"__managedcode\0\0\0"
        {
            return true;
        }

        false
    }
}

fn remove_libunwind(archive: &Path, patched: &Path) {
    let file = std::fs::File::open(archive).expect("failed to open built library");
    let mut archive = ar::Archive::new(std::io::BufReader::new(file));

    let patched = std::fs::File::create(patched).expect("failed to create patched library");
    let mut builder = ar::Builder::new(std::io::BufWriter::new(patched));

    while let Some(entry) = archive.next_entry() {
        let mut entry = entry.expect("reading library");
        if entry.header().identifier().starts_with(b"libunwind") {
            // remove libunwind
        } else {
            builder
                .append(&entry.header().clone(), &mut entry)
                .expect("copying file in archive");
        }
    }

    builder
        .into_inner()
        .unwrap()
        .flush()
        .expect("writing patched library");
}

fn create_libbootstrapperdll_a(obj: &Path, folder: &Path, target_info: &TargetInformation) {
    let lib_path = folder.join("libbootstrapperdll.a");
    let file = std::fs::File::create(&lib_path).expect("failed to create libbootstrapperdll.a");
    let mut builder = ar::Builder::new(std::io::BufWriter::new(file));
    builder
        .append_file(
            b"bootstrapperdll.o",
            &mut std::fs::File::open(obj).expect("opening bootstrapperdll.o"),
        )
        .unwrap();

    builder
        .into_inner()
        .unwrap()
        .flush()
        .expect("writing patched libbootstrapperdll.a");

    if target_info.family == TargetFamily::MacOS {
        // for bsd, ranlib to index
        Command::new("ranlib")
            .arg(lib_path)
            .status()
            .expect("running ranlib");
    }
}

fn create_libbootstrapperdll_lib(obj: &Path, folder: &Path, _target_info: &TargetInformation) {
    let lib_path = folder.join("bootstrapperdll.lib");

    cc::windows_registry::find(std::env::var("TARGET").unwrap().as_str(), "lib.exe")
        .expect("finding lib.exe")
        .arg(format!("/out:{}", lib_path.to_str().unwrap()))
        .arg(obj)
        .status()
        .expect("running lib /out:bootstrapperdll.lib bootstrapperdll.obj");
}
