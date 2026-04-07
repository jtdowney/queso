const std = @import("std");
const builtin = @import("builtin");

const ERTS_PAYLOAD = @embedFile("erts.tar.zst");
const APP_PAYLOAD = @embedFile("app.tar.zst");
const config = @import("config.zig");

fn getCacheDir(allocator: std.mem.Allocator) ![]const u8 {
    switch (builtin.os.tag) {
        .macos => {
            const home = std.posix.getenv("HOME") orelse return error.NoCacheDir;
            return std.fmt.allocPrint(allocator, "{s}/Library/Caches", .{home});
        },
        .windows => {
            return std.process.getEnvVarOwned(allocator, "LOCALAPPDATA") catch return error.NoCacheDir;
        },
        else => {
            if (std.posix.getenv("XDG_CACHE_HOME")) |xdg| {
                return allocator.dupe(u8, xdg);
            }
            const home = std.posix.getenv("HOME") orelse return error.NoCacheDir;
            return std.fmt.allocPrint(allocator, "{s}/.cache", .{home});
        },
    }
}

pub fn main() void {
    run() catch |err| {
        const stderr = std.fs.File.stderr();
        stderr.writeAll("queso: ") catch {};
        stderr.writeAll(@errorName(err)) catch {};
        stderr.writeAll("\n") catch {};
        std.process.exit(1);
    };
}

fn run() !void {
    var gpa: std.heap.GeneralPurposeAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const cache_dir = try getCacheDir(allocator);
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

    if (!dirExists(erts_dir)) {
        try extract(allocator, ERTS_PAYLOAD, erts_dir);
    }

    if (!dirExists(app_dir)) {
        try extract(allocator, APP_PAYLOAD, app_dir);
    }

    cleanStaleCache(base_dir);

    try bootErts(allocator, erts_dir, app_dir);
}

fn dirExists(path: []const u8) bool {
    var dir = std.fs.cwd().openDir(path, .{}) catch return false;
    dir.close();
    return true;
}

fn cleanStaleCache(base_dir: []const u8) void {
    const current_erts = "erts_" ++ config.erts_hash;
    const current_app = "app_" ++ config.app_hash;

    var dir = std.fs.cwd().openDir(base_dir, .{ .iterate = true }) catch return;
    defer dir.close();

    var iter = dir.iterate();
    while (iter.next() catch return) |entry| {
        if (entry.kind != .directory) continue;

        const is_stale = (std.mem.startsWith(u8, entry.name, "erts_") and
            !std.mem.eql(u8, entry.name, current_erts)) or
            (std.mem.startsWith(u8, entry.name, "app_") and
                !std.mem.eql(u8, entry.name, current_app));

        if (is_stale) {
            dir.deleteTree(entry.name) catch {};
        }
    }
}

fn extract(allocator: std.mem.Allocator, payload: []const u8, install_dir: []const u8) !void {
    const parent = std.fs.path.dirname(install_dir) orelse ".";
    const rand_hex: [16]u8 = std.fmt.hex(std.crypto.random.int(u64));
    const tmp_dir_path = try std.fmt.allocPrint(allocator, "{s}/.tmp_{s}", .{ parent, rand_hex });
    defer allocator.free(tmp_dir_path);

    try std.fs.cwd().makePath(tmp_dir_path);
    errdefer std.fs.cwd().deleteTree(tmp_dir_path) catch {};

    var dest_dir = try std.fs.cwd().openDir(tmp_dir_path, .{});
    defer dest_dir.close();

    var payload_reader: std.Io.Reader = .fixed(payload);
    const zstd = std.compress.zstd;
    const buf = try allocator.alloc(u8, zstd.default_window_len + zstd.block_size_max);
    defer allocator.free(buf);
    var decompress: zstd.Decompress = .init(&payload_reader, buf, .{});

    try std.tar.pipeToFileSystem(dest_dir, &decompress.reader, .{});

    std.fs.cwd().rename(tmp_dir_path, install_dir) catch {
        if (dirExists(install_dir)) {
            std.fs.cwd().deleteTree(tmp_dir_path) catch {};
            return;
        }

        const stderr = std.fs.File.stderr();
        stderr.writeAll("queso: failed to install cache at ") catch {};
        stderr.writeAll(install_dir) catch {};
        stderr.writeAll("\n") catch {};
        return error.RenameFailed;
    };
}

fn addLibPaths(
    allocator: std.mem.Allocator,
    argv: *std.ArrayListUnmanaged([]const u8),
    lib_dir: []const u8,
) !void {
    var dir = std.fs.cwd().openDir(lib_dir, .{ .iterate = true }) catch return;
    defer dir.close();

    var names: std.ArrayListUnmanaged([]const u8) = .empty;
    defer {
        for (names.items) |n| allocator.free(n);
        names.deinit(allocator);
    }

    var iter = dir.iterate();
    while (try iter.next()) |entry| {
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

        if (std.fs.cwd().access(ebin_path, .{})) |_| {
            try argv.append(allocator, "-pa");
            try argv.append(allocator, ebin_path);
        } else |_| {
            allocator.free(ebin_path);
        }
    }
}

fn bootErts(allocator: std.mem.Allocator, erts_dir: []const u8, app_dir: []const u8) !void {
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

    try addLibPaths(allocator, &argv, app_lib_dir);

    const eval_arg = try std.fmt.allocPrint(allocator, "'{s}':main()", .{config.entry_module});

    try argv.append(allocator, "-noshell");
    try argv.append(allocator, "-eval");
    try argv.append(allocator, eval_arg);
    try argv.append(allocator, "-s");
    try argv.append(allocator, "erlang");
    try argv.append(allocator, "halt");

    var proc_args = try std.process.argsAlloc(allocator);
    defer std.process.argsFree(allocator, proc_args);
    if (proc_args.len > 1) {
        try argv.append(allocator, "-extra");
        for (proc_args[1..]) |arg| {
            try argv.append(allocator, arg);
        }
    }

    var env_map = try std.process.getEnvMap(allocator);

    try env_map.put("ROOTDIR", erts_dir);
    try env_map.put("BINDIR", erts_bin_dir);
    try env_map.put("EMU", "beam");
    try env_map.put("PROGNAME", "erl");

    if (builtin.os.tag == .windows) {
        var child = std.process.Child.init(argv.items, allocator);
        child.env_map = &env_map;
        const term = try child.spawnAndWait();
        std.process.exit(switch (term) {
            .Exited => |code| code,
            else => 1,
        });
    } else {
        return std.process.execve(allocator, argv.items, &env_map);
    }
}
