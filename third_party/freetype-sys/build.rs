use std::env::VarError;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn add_sources(build: &mut cc::Build, root: &str, files: &[&str]) {
    let root = Path::new(root);
    build.files(files.iter().map(|src| {
        let mut p = root.join(src);
        p.set_extension("c");
        p
    }));

    build.include(root);
}

fn configure_freetype_build(build: &mut cc::Build) {
    build
        .warnings(false)
        .include(".")
        .include("freetype2/include")
        .include("libpng")
        .define("FT2_BUILD_LIBRARY", None)
        .define("FT_CONFIG_OPTION_USE_PNG", None);
}

fn supports_unix_mmap(out_dir: &Path) -> bool {
    if env::var_os("CARGO_CFG_UNIX").is_none() {
        return false;
    }

    let probe = out_dir.join("ftsystem_mmap_probe.c");
    fs::write(
        &probe,
        r#"
#include <unistd.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/types.h>

int cc_probe(const char *path) {
  int fd = open(path, O_RDONLY);
  struct stat stat_buf;
  void *base;

  if (fd < 0)
    return 0;
  if (fstat(fd, &stat_buf) < 0) {
    close(fd);
    return 0;
  }

  base = mmap(0, 4096, PROT_READ, MAP_PRIVATE, fd, 0);
  if (base != MAP_FAILED)
    munmap(base, 4096);
  close(fd);
  return 0;
}
"#,
    )
    .unwrap();

    let mut build = cc::Build::new();
    configure_freetype_build(&mut build);
    build
        .file(probe)
        .cargo_metadata(false)
        .cargo_warnings(false)
        .cargo_output(false)
        .try_compile("ftsystem_mmap_probe")
        .is_ok()
}

fn main() {
    if !cfg!(feature = "bundled") {
        let target = env::var("TARGET").unwrap();
        if !target.contains("android") && !target.contains("ohos") {
            pkg_config::Config::new()
                .atleast_version("24.3.18")
                .probe("freetype2")
                .unwrap();
        }
        return;
    }

    let mut build = cc::Build::new();
    configure_freetype_build(&mut build);

    add_sources(
        &mut build,
        "freetype2/src",
        &[
            "autofit/autofit",
            "base/ftbase",
            "base/ftbbox",
            "base/ftbdf",
            "base/ftbitmap",
            "base/ftcid",
            "base/ftdebug",
            "base/ftfstype",
            "base/ftgasp",
            "base/ftglyph",
            "base/ftgxval",
            "base/ftinit",
            "base/ftmm",
            "base/ftotval",
            "base/ftpatent",
            "base/ftpfr",
            "base/ftstroke",
            "base/ftsynth",
            "base/fttype1",
            "base/ftwinfnt",
            "bdf/bdf",
            "bzip2/ftbzip2",
            "cache/ftcache",
            "cff/cff",
            "cid/type1cid",
            "gzip/ftgzip",
            "lzw/ftlzw",
            "pcf/pcf",
            "pfr/pfr",
            "psaux/psaux",
            "pshinter/pshinter",
            "psnames/psnames",
            "raster/raster",
            "sdf/sdf",
            "svg/svg",
            "sfnt/sfnt",
            "smooth/smooth",
            "truetype/truetype",
            "type1/type1",
            "type42/type42",
            "winfonts/winfnt",
        ],
    );

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    if supports_unix_mmap(&out_dir) {
        build
            .define("HAVE_UNISTD_H", Some("1"))
            .define("HAVE_FCNTL_H", Some("1"))
            .file("freetype2/builds/unix/ftsystem.c");
    } else if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        build.file("freetype2/builds/windows/ftsystem.c");
    } else {
        build.file("freetype2/src/base/ftsystem.c");
    }

    build.compile("freetype2");

    // libz-sys comma separates multiple include paths.
    let zlib_include_paths = match env::var("DEP_Z_INCLUDE") {
        Ok(include_paths) => include_paths.split(",").map(String::from).collect(),
        Err(VarError::NotPresent) => {
            // For some targets (e.g. android and -ohos), libz-sys does not emit the variable,
            // but we expect the header to be in the sysroot instead so it's fine.
            vec![]
        }
        Err(VarError::NotUnicode(_os_str_paths)) => {
            println!("cargo:warning:libz-sys header files at non-unicode path. Ignoring");
            vec![]
        }
    };

    let mut build = cc::Build::new();
    build.include("libpng").includes(zlib_include_paths);
    build
        .file("libpng/png.c")
        .file("libpng/pngerror.c")
        .file("libpng/pngget.c")
        .file("libpng/pngmem.c")
        .file("libpng/pngpread.c")
        .file("libpng/pngread.c")
        .file("libpng/pngrio.c")
        .file("libpng/pngrtran.c")
        .file("libpng/pngrutil.c")
        .file("libpng/pngset.c")
        .file("libpng/pngtrans.c")
        .file("libpng/pngwio.c")
        .file("libpng/pngwrite.c")
        .file("libpng/pngwtran.c")
        .file("libpng/pngwutil.c");

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    if arch == "arm" || arch == "aarch64" {
        build
            .file("libpng/arm/arm_init.c")
            .file("libpng/arm/filter_neon_intrinsics.c")
            .file("libpng/arm/filter_neon.S")
            .file("libpng/arm/palette_neon_intrinsics.c");
    }

    build.compile("libpng.a");
}
