#!/usr/bin/env bash
# Build a musl-compatible static libcap and expose it via pkg-config.
#
# This mirrors openai/codex's `.github/scripts/install-musl-build-tools.sh`
# (libcap portion). The system libcap from apt is compiled against glibc and
# uses fortified symbols (`__sprintf_chk`, `__isoc23_strtoul`, ...) that do not
# exist in musl, so linking it into a musl binary fails. To stay faithful to the
# codex build we instead compile libcap from source with the musl toolchain and
# point pkg-config at the result so nothing resolves the host glibc libcap.
set -euo pipefail

: "${TARGET:?TARGET environment variable is required}"
: "${GITHUB_ENV:?GITHUB_ENV environment variable is required}"

case "${TARGET}" in
  x86_64-unknown-linux-musl)
    arch="x86_64"
    ;;
  aarch64-unknown-linux-musl)
    arch="aarch64"
    ;;
  *)
    echo "Unexpected musl target: ${TARGET}" >&2
    exit 1
    ;;
esac

libcap_version="2.75"
libcap_sha256="de4e7e064c9ba451d5234dd46e897d7c71c96a9ebf9a0c445bc04f4742d83632"
libcap_tarball_name="libcap-${libcap_version}.tar.xz"
libcap_download_url="https://mirrors.edge.kernel.org/pub/linux/libs/security/linux-privs/libcap2/${libcap_tarball_name}"

# Use the musl toolchain as the Rust linker to avoid the host glibc CRT.
if command -v "${arch}-linux-musl-gcc" >/dev/null; then
  musl_linker="$(command -v "${arch}-linux-musl-gcc")"
elif command -v musl-gcc >/dev/null; then
  musl_linker="$(command -v musl-gcc)"
else
  echo "musl gcc not found after install; arch=${arch}" >&2
  exit 1
fi

runner_temp="${RUNNER_TEMP:-/tmp}"
tool_root="${runner_temp}/autoreport-musl-tools-${TARGET}"
mkdir -p "${tool_root}"

libcap_root="${tool_root}/libcap-${libcap_version}"
libcap_src_root="${libcap_root}/src"
libcap_prefix="${libcap_root}/prefix"
libcap_pkgconfig_dir="${libcap_prefix}/lib/pkgconfig"

if [[ ! -f "${libcap_prefix}/lib/libcap.a" ]]; then
  mkdir -p "${libcap_src_root}" "${libcap_prefix}/lib" "${libcap_prefix}/include/sys" "${libcap_prefix}/include/linux" "${libcap_pkgconfig_dir}"
  libcap_tarball="${libcap_root}/${libcap_tarball_name}"

  curl -fsSL "${libcap_download_url}" -o "${libcap_tarball}"
  echo "${libcap_sha256}  ${libcap_tarball}" | sha256sum -c -

  tar -xJf "${libcap_tarball}" -C "${libcap_src_root}"
  libcap_source_dir="${libcap_src_root}/libcap-${libcap_version}"
  make -C "${libcap_source_dir}/libcap" -j"$(nproc)" \
    CC="${musl_linker}" \
    AR=ar \
    RANLIB=ranlib

  cp "${libcap_source_dir}/libcap/libcap.a" "${libcap_prefix}/lib/libcap.a"
  cp "${libcap_source_dir}/libcap/include/uapi/linux/capability.h" "${libcap_prefix}/include/linux/capability.h"
  cp "${libcap_source_dir}/libcap/include/sys/capability.h" "${libcap_prefix}/include/sys/capability.h"

  cat > "${libcap_pkgconfig_dir}/libcap.pc" <<EOF
prefix=${libcap_prefix}
exec_prefix=\${prefix}
libdir=\${prefix}/lib
includedir=\${prefix}/include

Name: libcap
Description: Linux capabilities
Version: ${libcap_version}
Libs: -L\${libdir} -lcap
Cflags: -I\${includedir}
EOF
fi

cc="${musl_linker}"

cflags="-pthread"
cxxflags="-pthread"

echo "CFLAGS=${cflags}" >> "$GITHUB_ENV"
echo "CXXFLAGS=${cxxflags}" >> "$GITHUB_ENV"
echo "CC=${cc}" >> "$GITHUB_ENV"
echo "TARGET_CC=${cc}" >> "$GITHUB_ENV"
target_cc_var="CC_${TARGET}"
target_cc_var="${target_cc_var//-/_}"
echo "${target_cc_var}=${cc}" >> "$GITHUB_ENV"

cargo_linker_var="CARGO_TARGET_${TARGET^^}_LINKER"
cargo_linker_var="${cargo_linker_var//-/_}"
echo "${cargo_linker_var}=${musl_linker}" >> "$GITHUB_ENV"

# Allow pkg-config resolution during cross-compilation.
echo "PKG_CONFIG_ALLOW_CROSS=1" >> "$GITHUB_ENV"
echo "PKG_CONFIG_PATH=${libcap_pkgconfig_dir}" >> "$GITHUB_ENV"
pkg_config_path_var="PKG_CONFIG_PATH_${TARGET}"
pkg_config_path_var="${pkg_config_path_var//-/_}"
echo "${pkg_config_path_var}=${libcap_pkgconfig_dir}" >> "$GITHUB_ENV"
pkg_config_libdir_var="PKG_CONFIG_LIBDIR_${TARGET}"
pkg_config_libdir_var="${pkg_config_libdir_var//-/_}"
# Do not let musl cross-builds resolve native libraries from the host glibc
# pkg-config directories. libcap is the only target package provided here.
echo "${pkg_config_libdir_var}=${libcap_pkgconfig_dir}" >> "$GITHUB_ENV"

echo "Installed musl libcap at ${libcap_prefix}"
