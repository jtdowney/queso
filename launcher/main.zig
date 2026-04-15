const std = @import("std");
const builtin = @import("builtin");

const ERTS_PAYLOAD = @embedFile("erts.tar.zst");
const APP_PAYLOAD = @embedFile("app.tar.zst");
const config = @import("config.zig");

const Io = std.Io;
const Dir = Io.Dir;

fn getCacheDir(allocator: std.mem.Allocator, env: *const std.process.Environ.Map) ![]const u8 {
    switch (builtin.os.tag) {
        .macos => {
            const home = env.get("HOME") orelse return error.NoCacheDir;
            return std.fmt.allocPrint(allocator, "{s}/Library/Caches", .{home});
        },
        .windows => {
            const local_app_data = env.get("LOCALAPPDATA") orelse return error.NoCacheDir;
            return allocator.dupe(u8, local_app_data);
        },
        else => {
            if (env.get("XDG_CACHE_HOME")) |xdg| {
                return allocator.dupe(u8, xdg);
            }
            const home = env.get("HOME") orelse return error.NoCacheDir;
            return std.fmt.allocPrint(allocator, "{s}/.cache", .{home});
        },
    }
}

pub fn main(init: std.process.Init) !void {
    const allocator = init.gpa;
    const io = init.io;

    const cache_dir = try getCacheDir(allocator, init.environ_map);
    defer allocator.free(cache_dir);

    const base_dir = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ cache_dir, config.name });
    defer allocator.free(base_dir);

    const erts_dir = try std.fmt.allocPrint(
        allocator,
        "{s}/erts_{s}",
        .{ base_dir, config.erts_hash },
    );
    defer allocator.free(erts_dir);

    const app_dir = try std.fmt.allocPrint(
        allocator,
        "{s}/app_{s}",
        .{ base_dir, config.app_hash },
    );
    defer allocator.free(app_dir);

    if (!dirExists(io, erts_dir)) {
        try extract(allocator, io, ERTS_PAYLOAD, erts_dir);
    }

    if (!dirExists(io, app_dir)) {
        try extract(allocator, io, APP_PAYLOAD, app_dir);
    }

    cleanStaleCache(io, base_dir);

    try bootErts(allocator, init, erts_dir, app_dir);
}

fn dirExists(io: Io, path: []const u8) bool {
    const dir = Dir.cwd().openDir(io, path, .{}) catch return false;
    dir.close(io);
    return true;
}

fn cleanStaleCache(io: Io, base_dir: []const u8) void {
    const current_erts = "erts_" ++ config.erts_hash;
    const current_app = "app_" ++ config.app_hash;

    const dir = Dir.cwd().openDir(io, base_dir, .{ .iterate = true }) catch return;
    defer dir.close(io);

    var iter = dir.iterate();
    while (iter.next(io) catch return) |entry| {
        if (entry.kind != .directory) continue;

        const is_stale = (std.mem.startsWith(u8, entry.name, "erts_") and
            !std.mem.eql(u8, entry.name, current_erts)) or
            (std.mem.startsWith(u8, entry.name, "app_") and
                !std.mem.eql(u8, entry.name, current_app));

        if (is_stale) {
            dir.deleteTree(io, entry.name) catch {};
        }
    }
}

fn extract(allocator: std.mem.Allocator, io: Io, payload: []const u8, install_dir: []const u8) !void {
    const cwd = Dir.cwd();
    const parent = std.fs.path.dirname(install_dir) orelse ".";
    var rand_bytes: [8]u8 = undefined;
    io.random(&rand_bytes);
    const rand_hex: [16]u8 = std.fmt.hex(std.mem.readInt(u64, &rand_bytes, .little));
    const tmp_dir_path = try std.fmt.allocPrint(allocator, "{s}/.tmp_{s}", .{ parent, rand_hex });
    defer allocator.free(tmp_dir_path);

    try cwd.createDirPath(io, tmp_dir_path);
    errdefer cwd.deleteTree(io, tmp_dir_path) catch {};

    const dest_dir = try cwd.openDir(io, tmp_dir_path, .{});
    defer dest_dir.close(io);

    var payload_reader: Io.Reader = .fixed(payload);
    const zstd = std.compress.zstd;
    const buf = try allocator.alloc(u8, zstd.default_window_len + zstd.block_size_max);
    defer allocator.free(buf);
    var decompress: zstd.Decompress = .init(&payload_reader, buf, .{});

    try std.tar.pipeToFileSystem(io, dest_dir, &decompress.reader, .{});

    Dir.rename(cwd, tmp_dir_path, cwd, install_dir, io) catch {
        if (dirExists(io, install_dir)) {
            cwd.deleteTree(io, tmp_dir_path) catch {};
            return;
        }

        std.log.err("failed to install cache at {s}", .{install_dir});
        return error.RenameFailed;
    };
}

fn addLibPaths(
    allocator: std.mem.Allocator,
    io: Io,
    argv: *std.ArrayListUnmanaged([]const u8),
    lib_dir: []const u8,
) !void {
    const dir = Dir.cwd().openDir(io, lib_dir, .{ .iterate = true }) catch return;
    defer dir.close(io);

    var names: std.ArrayListUnmanaged([]const u8) = .empty;
    defer {
        for (names.items) |n| allocator.free(n);
        names.deinit(allocator);
    }

    var iter = dir.iterate();
    while (try iter.next(io)) |entry| {
        if (entry.kind != .directory) continue;
        try names.append(allocator, try allocator.dupe(u8, entry.name));
    }

    std.mem.sort([]const u8, names.items, {}, struct {
        fn cmp(_: void, a: []const u8, b: []const u8) bool {
            return std.mem.order(u8, a, b) == .lt;
        }
    }.cmp);

    for (names.items) |name| {
        const ebin_path = try std.fmt.allocPrint(allocator, "{s}/{s}/ebin", .{ lib_dir, name });

        if (Dir.cwd().access(io, ebin_path, .{})) |_| {
            try argv.append(allocator, "-pa");
            try argv.append(allocator, ebin_path);
        } else |_| {
            allocator.free(ebin_path);
        }
    }
}

fn bootErts(allocator: std.mem.Allocator, init: std.process.Init, erts_dir: []const u8, app_dir: []const u8) !void {
    const erts_bin_dir = try std.fmt.allocPrint(
        allocator,
        "{s}/erts-{s}/bin",
        .{ erts_dir, config.erts_version },
    );

    const erl_name = if (builtin.os.tag == .windows) "erl.exe" else "erlexec";
    const erl = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ erts_bin_dir, erl_name });

    const app_lib_dir = try std.fmt.allocPrint(allocator, "{s}/lib", .{app_dir});

    const boot_file = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ erts_dir, config.boot_path });

    var argv: std.ArrayListUnmanaged([]const u8) = .empty;
    try argv.append(allocator, erl);
    try argv.append(allocator, "-boot");
    try argv.append(allocator, boot_file);

    try addLibPaths(allocator, init.io, &argv, app_lib_dir);

    const eval_arg = try std.fmt.allocPrint(allocator, "'{s}':main()", .{config.entry_module});

    try argv.append(allocator, "-noshell");
    try argv.append(allocator, "-eval");
    try argv.append(allocator, eval_arg);
    try argv.append(allocator, "-s");
    try argv.append(allocator, "erlang");
    try argv.append(allocator, "halt");

    var args_iter = std.process.Args.Iterator.init(init.minimal.args);
    _ = args_iter.skip();
    var has_extra = false;
    while (args_iter.next()) |arg| {
        if (!has_extra) {
            try argv.append(allocator, "-extra");
            has_extra = true;
        }
        try argv.append(allocator, arg);
    }

    const env_map = init.environ_map;

    try env_map.put("ROOTDIR", erts_dir);
    try env_map.put("BINDIR", erts_bin_dir);
    try env_map.put("EMU", "beam");
    try env_map.put("PROGNAME", "erl");

    if (builtin.os.tag == .windows) {
        var child = try std.process.spawn(init.io, .{
            .argv = argv.items,
            .environ_map = env_map,
        });
        const term = try child.wait(init.io);
        std.process.exit(switch (term) {
            .exited => |code| code,
            else => 1,
        });
    } else {
        return std.process.replace(init.io, .{
            .argv = argv.items,
            .environ_map = env_map,
        });
    }
}
