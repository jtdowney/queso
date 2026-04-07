#!/usr/bin/env escript
%% -*- erlang -*-

main(_Args) ->
    loop().

loop() ->
    case io:read('') of
        {ok, Term} ->
            try handle(Term)
            catch Class:Reason ->
                respond_error(io_lib:format("~p:~p", [Class, Reason]))
            end,
            loop();
        eof ->
            ok;
        {error, {_, _, Desc}} ->
            respond_error(io_lib:format("parse error: ~s", [erl_parse:format_error(Desc)])),
            loop();
        {error, Reason} ->
            respond_error(io_lib:format("read error: ~p", [Reason])),
            loop()
    end.

handle(get_otp_version) ->
    OtpRelease = erlang:system_info(otp_release),
    ReleaseDir = filename:join([code:root_dir(), "releases", OtpRelease, "OTP_VERSION"]),
    Version = case file:read_file(ReleaseDir) of
        {ok, V} -> string:trim(V);
        _ -> io_lib:format("~s.0", [OtpRelease])
    end,
    respond(Version);

handle({strip_beam, TmpDir, Paths}) ->
    lists:foldl(
        fun(Path, Index) ->
            {ok, Bin} = file:read_file(Path),
            Stripped = case beam_lib:strip(Bin) of
                {ok, {_, S}} -> S;
                _ -> Bin
            end,
            OutFile = filename:join(TmpDir, integer_to_list(Index)),
            ok = file:write_file(OutFile, Stripped),
            Index + 1
        end,
        0,
        Paths
    ),
    respond("ok");

handle({parse_app_files, Dir}) ->
    case file:list_dir(Dir) of
        {ok, Entries} ->
            Apps = lists:filtermap(
                fun(Entry) ->
                    parse_app_dir(filename:join(Dir, Entry))
                end,
                lists:sort(Entries)
            ),
            Line = lists:join(";", Apps),
            respond(Line);
        {error, Reason} ->
            respond_error(io_lib:format("cannot list ~s: ~p", [Dir, Reason]))
    end;

handle(Other) ->
    respond_error(io_lib:format("unknown command: ~p", [Other])).

parse_app_dir(AppDir) ->
    Ebin = filename:join(AppDir, "ebin"),
    case file:list_dir(Ebin) of
        {ok, Files} ->
            AppFiles = [F || F <- Files, filename:extension(F) =:= ".app"],
            case AppFiles of
                [AppFile | _] ->
                    parse_single_app(filename:join(Ebin, AppFile));
                [] ->
                    false
            end;
        {error, _} ->
            false
    end.

parse_single_app(Path) ->
    case file:consult(Path) of
        {ok, [{application, Name, Props}]} ->
            Apps = proplists:get_value(applications, Props, []),
            Included = proplists:get_value(included_applications, Props, []),
            AllDeps = Apps ++ Included,
            DepStr = lists:join(",", [atom_to_list(D) || D <- AllDeps]),
            {true, io_lib:format("~s:~s", [Name, DepStr])};
        _ ->
            false
    end.

respond(Data) ->
    io:format("~s~n", [Data]).

respond_error(Msg) ->
    io:format("ERROR: ~s~n", [Msg]).
