cmake_minimum_required(VERSION 3.18)
project(anvil VERSION 0.0.1 LANGUAGES CXX)

# ─────────────────────────────────────────────────────────────
#  Global settings
# ─────────────────────────────────────────────────────────────

set(CMAKE_CXX_STANDARD          20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS        OFF)

# Release by default
if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release)
endif()

# Hardening flags
add_compile_options(
    -Wall -Wextra -Wpedantic
    -fstack-protector-strong
    -D_FORTIFY_SOURCE=2
    -fPIC
)

if(CMAKE_BUILD_TYPE STREQUAL "Release")
    add_compile_options(-O2)
endif()

# ─────────────────────────────────────────────────────────────
#  libanvil — static library (linked into hammer)
# ─────────────────────────────────────────────────────────────

add_library(anvil_static STATIC
    src/anvil_impl.cpp
)

target_include_directories(anvil_static
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        $<INSTALL_INTERFACE:include>
)

set_target_properties(anvil_static PROPERTIES
    OUTPUT_NAME anvil
    ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib
)

# ─────────────────────────────────────────────────────────────
#  anvil binary — /usr/bin/anvil
# ─────────────────────────────────────────────────────────────

add_executable(anvil_bin
    src/main.cpp
    src/anvil_impl.cpp
)

target_include_directories(anvil_bin PRIVATE include)

set_target_properties(anvil_bin PROPERTIES
    OUTPUT_NAME anvil
    RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin
)

target_compile_options(anvil_bin PRIVATE -DANVIL_STANDALONE)

# ─────────────────────────────────────────────────────────────
#  Install
# ─────────────────────────────────────────────────────────────

include(GNUInstallDirs)

install(TARGETS anvil_bin
    RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR}   # /usr/bin/anvil
)

install(TARGETS anvil_static
    ARCHIVE DESTINATION ${CMAKE_INSTALL_LIBDIR}   # /usr/lib/libanvil.a
)

install(FILES include/anvil.h
    DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}/anvil  # /usr/include/anvil/anvil.h
)

# ─────────────────────────────────────────────────────────────
#  Build instructions (comment)
# ─────────────────────────────────────────────────────────────

# cmake -B build -DCMAKE_BUILD_TYPE=Release
# cmake --build build -j$(nproc)
# sudo cmake --install build --prefix /usr
