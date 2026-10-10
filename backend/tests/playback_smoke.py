"""使用隔离实例验证服务端选轨, 活动租约和可续接 HLS 缓存."""

import argparse
import array
import hashlib
import json
import math
import struct
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path


def ffmpeg(*args):
    subprocess.run(["ffmpeg", "-hide_banner", "-v", "error", "-nostdin", "-y", *args], check=True)


def bitmap_fixture(path):
    # PGS 白色矩形在第 9 秒移位, 用画面差异量化跳转位置和字幕烧录效果.
    def segment(seconds, kind, data):
        timestamp = int(seconds * 90000)
        return b"PG" + struct.pack(">IIBH", timestamp, timestamp, kind, len(data)) + data

    output = bytearray()
    for number, (seconds, y) in enumerate([(0, 140), (9, 10), (20, None)]):
        pcs = struct.pack(">HHBHBBBB", 320, 180, 0x20, number, 0x80, 0, 0, int(y is not None))
        if y is not None:
            pcs += struct.pack(">HBBHH", 0, 0, 0, 128, y)
        output.extend(segment(seconds, 0x16, pcs))
        if y is not None:
            output.extend(segment(seconds, 0x17, struct.pack(">BBHHHH", 1, 0, 128, y, 64, 16)))
            output.extend(segment(seconds, 0x14, bytes([0, 0, 1, 235, 128, 128, 255])))
            pixels = (bytes([1]) * 64 + b"\x00\x00") * 16
            data = struct.pack(">HH", 64, 16) + pixels
            output.extend(segment(seconds, 0x15, b"\x00\x00\x00\xc0" + len(data).to_bytes(3, "big") + data))
        output.extend(segment(seconds, 0x80, b""))
    path.write_bytes(output)


def fixtures(directory, font):
    directory.mkdir(parents=True, exist_ok=True)
    ffmpeg(
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x180:rate=24",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
        "-t",
        "20",
        "-c:v",
        "libx264",
        "-threads",
        "1",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "96",
        "-c:a",
        "aac",
        "-movflags",
        "+faststart",
        str(directory / "direct.mp4"),
    )
    source = str(directory / "direct.mp4")
    ffmpeg("-i", source, "-c", "copy", str(directory / "remux.mkv"))
    ffmpeg("-i", source, "-c:v", "copy", "-c:a", "flac", str(directory / "flac.mkv"))
    ffmpeg(
        "-i",
        source,
        "-c:v",
        "libx264",
        "-threads",
        "1",
        "-pix_fmt",
        "yuv420p10le",
        "-c:a",
        "copy",
        str(directory / "h264-10bit.mkv"),
    )
    ffmpeg(
        "-i",
        source,
        "-c:v",
        "libx265",
        "-pix_fmt",
        "yuv420p10le",
        "-x265-params",
        "pools=1:frame-threads=1:log-level=error",
        "-c:a",
        "copy",
        str(directory / "hevc.mkv"),
    )
    ffmpeg("-i", source, "-an", "-c:v", "copy", str(directory / "silent.mkv"))
    ffmpeg("-f", "lavfi", "-i", "testsrc2=size=320x180:rate=120", "-t", "3",
        "-c:v", "libx264", "-threads", "1", str(directory / "high-fps.mp4"))
    for name, rate in [("sixty-fps.mkv", "60"), ("fractional-fps.mkv", "60000/1001")]:
        ffmpeg("-f", "lavfi", "-i", f"testsrc2=size=320x180:rate={rate}", "-t", "3",
            "-c:v", "libx264", "-threads", "1", str(directory / name))
    subtitle = Path(__file__).parent / "fixtures" / "sample.ass"
    srt = Path(__file__).parent / "fixtures" / "sample.srt"
    ffmpeg(
        "-i",
        source,
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=880:sample_rate=48000",
        "-i",
        str(subtitle),
        "-i",
        str(srt),
        "-t",
        "20",
        "-map",
        "0:v",
        "-map",
        "0:a",
        "-map",
        "1:a",
        "-map",
        "2:s",
        "-map",
        "3:s",
        "-c:v",
        "copy",
        "-c:a",
        "aac",
        "-c:s:0",
        "ass",
        "-c:s:1",
        "srt",
        "-metadata:s:a:0",
        "language=eng",
        "-metadata:s:a:1",
        "language=jpn",
        "-attach",
        str(font),
        "-metadata:s:t:0",
        "mimetype=application/x-truetype-font",
        str(directory / "tracks.mkv"),
    )
    ffmpeg(
        "-itsoffset", "5", "-i", str(directory / "tracks.mkv"), "-map", "0", "-c", "copy", str(directory / "offset.mkv")
    )
    ffmpeg("-stream_loop", "29", "-i", str(directory / "tracks.mkv"), "-map", "0", "-c", "copy", str(directory / "long.mkv"))
    bitmap_fixture(directory / "sample.sup")
    ffmpeg(
        "-i",
        source,
        "-i",
        str(directory / "sample.sup"),
        "-map",
        "0:v",
        "-map",
        "0:a",
        "-map",
        "1:s",
        "-c",
        "copy",
        str(directory / "bitmap.mkv"),
    )
    # 延长容器内字幕样本, 保证测试能经历多次真实停止和续接, 而不是过早到达 EOF.
    for name in ["tracks", "offset", "bitmap"]:
        ffmpeg("-copyts", "-stream_loop", "2", "-i", str(directory / f"{name}.mkv"),
            "-map", "0", "-c", "copy", str(directory / f"resume-{name}.mkv"))
    ffmpeg("-f", "lavfi", "-i", "testsrc2=size=320x180:rate=60000/1001",
        "-f", "lavfi", "-i", "aevalsrc=0.35*sin(2*PI*(317*t+13*t*t))+0.1*sin(2*PI*1049*t):s=44100",
        "-vf", "drawbox=x=0:y=0:w=120:h=90:color=white:t=fill:enable='mod(n,2)'",
        "-t", "64", "-c:v", "libx264", "-threads", "1", "-g", "137", "-c:a", "flac",
        str(directory / "resume-chirp.mkv"))
    ffmpeg("-i", str(directory / "resume-chirp.mkv"),
        "-vf", "select='not(eq(mod(n,17),0))'", "-fps_mode", "passthrough",
        "-c:v", "libx264", "-threads", "1", "-g", "137", "-c:a", "copy",
        str(directory / "resume-vfr.mkv"))
    (directory / "direct.eng.srt").write_bytes(srt.read_bytes())
    (directory / "direct.zh.ass").write_bytes(subtitle.read_bytes())
    (directory / "invalid.mp4").write_bytes(b"intentionally invalid media for probe error tests")


class Client:
    def __init__(self, base, container, media, limit, thread_budget=16, idle_seconds=60):
        assert container != "anime-server", "Do not run destructive media tests against the production container"
        self.base = base.rstrip("/")
        self.container = container
        self.media = Path("/media") if container else media
        self.limit = limit
        self.thread_budget = thread_budget
        self.idle_seconds = idle_seconds
        self.sessions = []
        self.keys = {}
        self.suspended = set()

    def call(self, path, body=None, method="GET", headers=None, status=200):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            self.base + path, data=data, method=method, headers={"Content-Type": "application/json", **(headers or {})}
        )
        try:
            response = urllib.request.urlopen(req, timeout=130)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            content = response.read()
            assert response.status == status, (path, response.status, content.decode(errors="replace"))
            if "application/json" in response.headers.get("Content-Type", ""):
                return json.loads(content)
            return content

    def command(self, tool, *args, data=None):
        prefix = ["docker", "exec", "-i", self.container] if self.container else []
        result = subprocess.run([*prefix, tool, *args], input=data, capture_output=True, timeout=60, check=False)
        assert result.returncode == 0, result.stderr.decode(errors="replace")
        return result.stdout

    def plan(self, file, info, audio=1, subtitle=None, position=0):
        result = self.call(
            f"/api/files/{file}/playback",
            {
                "audio_stream_index": audio,
                "subtitle_stream_index": subtitle,
                "position": position,
            },
            "POST",
        )
        return self.register(file, result)

    def register(self, file, result):
        self.sessions.append(result["session_id"])
        config = {
            "src_file_pre16mib_md5": file, "video_stream": result["media"]["video_tracks"][0]["stream_index"],
            "audio_stream": result["selection"]["audio"], "subtitle_stream": result["selection"]["subtitle"],
            "encoder_profile": "h264-high-yuv420p-aac-fmp4-v2",
        }
        self.keys[result["session_id"]] = hashlib.md5(json.dumps(config, separators=(",", ":")).encode()).hexdigest()
        assert set(result) == {
            "session_id",
            "url",
            "mime_type",
            "start_time",
            "duration",
            "position",
            "complete",
            "generating",
            "stopping",
            "encoding_threads",
            "ready",
            "buffered_until",
            "error",
            "media",
            "selection",
        }, result
        assert result["mime_type"] == "application/vnd.apple.mpegurl"
        assert result["start_time"] == 0, result
        return result

    def ready(self, plan, position=0, complete=False):
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            state = self.call(f"/api/playback/{plan['session_id']}/status")
            assert state["error"] is None, state
            assert "state" not in state and "variant_key" not in state, state
            if state["ready"] and state["buffered_until"] > position and (
                not complete or state["complete"] and not state["generating"]
            ):
                return state
            time.sleep(0.2)
        raise AssertionError("playback never became ready")

    def cache_path(self, plan):
        return f"/cache/playback/{self.keys[plan['session_id']]}"

    def exists(self, path):
        result = subprocess.run(
            ["docker", "exec", self.container, "test", "-e", path], check=False
        )
        assert result.returncode in (0, 1)
        return result.returncode == 0

    def restart(self, seconds=40):
        subprocess.run(["docker", "restart", "--timeout", str(seconds), self.container],
            check=True, stdout=subprocess.DEVNULL)
        deadline = time.monotonic() + 90
        while True:
            try:
                self.call("/health")
                return
            except (urllib.error.URLError, ConnectionError):
                assert time.monotonic() < deadline, "server failed to restart"
                time.sleep(0.2)

    def release(self, plan, expect_cache=True):
        id = plan["session_id"]
        started = time.monotonic()
        self.call(f"/api/playback/{id}", method="DELETE", status=204)
        assert time.monotonic() - started < 2, "DELETE waited for background generation to stop"
        self.sessions.remove(id)
        self.call(f"/api/playback/{id}/status", status=404)
        if self.container and expect_cache:
            assert self.exists(self.cache_path(plan)), "releasing a lease deleted its cache"

    def playlist(self, plan):
        return self.command("cat", self.cache_path(plan) + "/index.m3u8").decode()

    def segment_names(self, plan):
        return [line for line in self.playlist(plan).splitlines() if line.endswith(".m4s")]

    def wait_idle(self, plan, seconds=40):
        deadline = time.monotonic() + seconds
        path = self.cache_path(plan)
        while True:
            if not self.exists(path):
                return
            work = self.command("find", path, "-maxdepth", "1", "-type", "d", "-name", ".work-*").strip()
            if not work:
                return
            assert time.monotonic() < deadline, "generation did not release its private working directory"
            time.sleep(0.1)

    def snapshot(self, plan):
        # 记录完整前缀的 inode 和摘要, 不用单纯的文件存在性代替未重编码验证.
        path = self.cache_path(plan)
        names = ["init.mp4", *self.segment_names(plan)]
        return {name: (
            self.command("stat", "-c", "%i:%s", path + "/" + name),
            self.command("sha256sum", path + "/" + name).split()[0],
        ) for name in names}

    def assert_prefix(self, plan, before):
        path = self.cache_path(plan)
        for name, (stamp, digest) in before.items():
            assert self.command("stat", "-c", "%i:%s", path + "/" + name) == stamp, name
            assert self.command("sha256sum", path + "/" + name).split()[0] == digest, name

    def workers(self):
        # 只检查指定隔离容器内的编码器, 不把校验或测试自身的解码进程算作编码器.
        result = self.command("sh", "-c", r"""
for p in /proc/[0-9]*; do
  test -r "$p/cmdline" || continue
  args=$(tr '\000' ' ' < "$p/cmdline")
  case "$args" in
    ffmpeg\ *|/usr/bin/ffmpeg\ *)
      case "$args" in
        *"-c:v libx264"*) printf '%s\t%s\t%s\n' "${p##*/}" "$(readlink "$p/cwd")" "$args";;
      esac;;
  esac
done
""")
        result = [line.split("\t", 2) for line in result.decode().splitlines()]
        return [{"pid": int(pid), "directory": directory, "threads": int(args.rsplit("-threads ", 1)[1].split()[0])}
            for pid, directory, args in result]

    def worker(self, plan, seconds=20):
        deadline = time.monotonic() + seconds
        while True:
            state = self.call(f"/api/playback/{plan['session_id']}/status")
            assert state["error"] is None, state
            workers = [worker for worker in self.workers() if worker["directory"].startswith(self.cache_path(plan) + "/")]
            assert len(workers) <= 1, "two encoders are writing the same variant"
            if workers:
                return workers[0]
            assert time.monotonic() < deadline, ("encoder did not start", self.call(f"/api/playback/{plan['session_id']}/status"))
            time.sleep(0.05)

    def signal(self, pid, signal):
        subprocess.run(["docker", "exec", self.container, "sh", "-c",
            'kill -"$1" "$2"', "sh", signal, str(pid)], check=True)
        if signal == "STOP":
            self.suspended.add(pid)
        else:
            self.suspended.discard(pid)

    def drain_staged(self, plan, worker):
        # 暂停编码器后先提交它已有的所有完整输出, 再测量等待下一分片的停止期限.
        previous = None
        stable = time.monotonic()
        while time.monotonic() - stable < 0.8:
            self.call(f"/api/playback/{plan['session_id']}/status")
            current = self.playlist(plan)
            if current != previous:
                previous = current
                stable = time.monotonic()
            time.sleep(0.1)
        assert any(value["pid"] == worker["pid"] for value in self.workers()), "paused encoder exited unexpectedly"

    def stream(self, plan, audio=True, position=0):
        playlist = self.call(plan["url"]).decode()
        assert '#EXT-X-MAP:URI="init.mp4"' in playlist
        assert "#EXT-X-INDEPENDENT-SEGMENTS" in playlist
        first = next(line for line in playlist.splitlines() if line.endswith(".m4s"))
        base = plan["url"].rsplit("/", 1)[0]
        assert len(self.call(f"{base}/{first}", headers={"Range": "bytes=0-31"}, status=206)) == 32
        data = self.call(f"{base}/init.mp4")
        duration = 0
        for line in playlist.splitlines():
            if line.startswith("#EXTINF:"):
                duration += float(line.split(":", 1)[1].split(",")[0])
            elif line.endswith(".m4s"):
                data += self.call(f"{base}/{line}")
                if duration > position + 0.5:
                    break
        streams = json.loads(
            self.command("ffprobe", "-v", "error", "-show_streams", "-of", "json", "-i", "pipe:0", data=data)
        )["streams"]
        video = next(stream for stream in streams if stream["codec_type"] == "video")
        assert video["codec_name"] == "h264" and video["pix_fmt"] == "yuv420p", video
        assert video["profile"] == "High" and (video["width"], video["height"]) == (320, 180), video
        assert len(streams) == (2 if audio else 1), streams
        if audio:
            sound = next(stream for stream in streams if stream["codec_type"] == "audio")
            assert (sound["codec_name"], sound["profile"], sound["channels"], sound["sample_rate"]) == (
                "aac",
                "LC",
                2,
                "48000",
            ), sound
        return data

    def frame(self, data, position=0):
        result = self.command(
            "ffmpeg",
            "-v",
            "error",
            "-i",
            "pipe:0",
            "-ss",
            str(position),
            "-frames:v",
            "1",
            "-vf",
            "format=gray",
            "-f",
            "rawvideo",
            "pipe:1",
            data=data,
        )
        assert len(result) == 320 * 180
        return result

    def frequency(self, data, expected):
        samples = array.array(
            "f",
            self.command(
                "ffmpeg",
                "-v",
                "error",
                "-i",
                "pipe:0",
                "-map",
                "0:a:0",
                "-ac",
                "1",
                "-ar",
                "48000",
                "-t",
                "1",
                "-f",
                "f32le",
                "pipe:1",
                data=data,
            ),
        )
        samples = samples[4800:28800]
        energy = {}
        for frequency in (440, 880):
            sine = sum(sample * math.sin(2 * math.pi * frequency * i / 48000) for i, sample in enumerate(samples))
            cosine = sum(sample * math.cos(2 * math.pi * frequency * i / 48000) for i, sample in enumerate(samples))
            energy[frequency] = sine * sine + cosine * cosine
        assert energy[expected] > energy[1320 - expected] * 100, energy

    def run(self):
        self.call("/api/scan", {}, "POST")
        files = {item["name"]: item["pre16mib_md5"] for item in self.call("/api/files")}
        required = [
            "direct.mp4",
            "remux.mkv",
            "flac.mkv",
            "h264-10bit.mkv",
            "hevc.mkv",
            "silent.mkv",
            "tracks.mkv",
            "offset.mkv",
            "bitmap.mkv",
            "long.mkv",
            "invalid.mp4",
            "high-fps.mp4",
            "sixty-fps.mkv",
            "fractional-fps.mkv",
            "resume-tracks.mkv",
            "resume-offset.mkv",
            "resume-bitmap.mkv",
            "resume-chirp.mkv",
            "resume-vfr.mkv",
        ]
        assert all(name in files for name in required), "Use an isolated server containing --fixtures output"
        info = {name: self.call(f"/api/files/{files[name]}/streams") for name in required if name != "invalid.mp4"}
        assert info["direct.mp4"]["subtitle_tracks"] == [], "external subtitles were exposed"
        assert all("source_version" not in value for value in info.values())
        for name in required[:6]:
            plan = self.ready(self.plan(files[name], info[name], audio=None if name == "silent.mkv" else 1))
            self.stream(plan, audio=name != "silent.mkv")
            self.release(plan)
            print("PASS unified H.264/AAC fMP4 HLS:", name)

        for name in ["high-fps.mp4", "sixty-fps.mkv", "fractional-fps.mkv"]:
            plan = self.ready(self.plan(files[name], info[name], audio=None), complete=True)
            data = self.stream(plan, audio=False)
            rate = json.loads(self.command("ffprobe", "-v", "error", "-select_streams", "v:0",
                "-count_frames", "-show_entries", "stream=nb_read_frames,duration",
                "-of", "json", "pipe:0", data=data))["streams"][0]
            assert int(rate["nb_read_frames"]) <= math.ceil(float(rate["duration"]) * 60)
            if name != "high-fps.mp4":
                assert int(rate["nb_read_frames"]) == 180, ("frames dropped below the rate limit", name, rate)
            self.release(plan)
        print("PASS 60 fps ceiling without dropping 60 fps or fractional-rate source frames")

        id = files["tracks.mkv"]
        tracks = info["tracks.mkv"]
        a = self.ready(self.plan(id, tracks, position=7.375), position=7.375)
        b = self.plan(id, tracks, position=7.375)
        assert a["session_id"] != b["session_id"] and a["url"] != b["url"]
        assert self.keys[a["session_id"]] == self.keys[b["session_id"]]
        data = self.stream(a, position=7.375)
        # 源画面按输出契约的有理数帧率采样, 不能把未重采样的 seek 帧当成同一时间格.
        source_start = float(json.loads(self.command("ffprobe", "-v", "error", "-show_entries",
            "format=start_time", "-of", "json", str(self.media / "tracks.mkv")))["format"]["start_time"])
        reference = self.command(
            "ffmpeg",
            "-v",
            "error",
            "-copyts",
            "-i",
            str(self.media / "tracks.mkv"),
            "-ss",
            "7.375",
            "-frames:v",
            "1",
            "-vf",
            f"settb=expr=1/90000,setpts=PTS-{round(source_start * 90000)},"
            "fps=fps='min(source_fps,60)':start_time=0,format=gray",
            "-f",
            "rawvideo",
            "pipe:1",
        )
        error = sum(abs(x - y) for x, y in zip(self.frame(data, 7.375), reference)) / len(reference)
        assert error < 4, ("seek frame differs from exact source frame", error)
        if self.container:
            decoded_hls = self.command("ffmpeg", "-v", "error", "-live_start_index", "0",
                "-i", "http://127.0.0.1:8080" + a["url"],
                "-ss", "7.375", "-frames:v", "1", "-vf", "format=gray", "-f", "rawvideo", "pipe:1")
            assert len(decoded_hls) == len(reference)
            error = sum(abs(x - y) for x, y in zip(decoded_hls, reference)) / len(reference)
            assert error < 4, ("HLS seek frame differs from exact source frame", error)
        self.frequency(data, 440)
        self.release(a)
        self.stream(self.ready(b))
        self.release(b)
        alternate = self.ready(self.plan(id, tracks, audio=2, position=7.375), position=7.375)
        self.frequency(self.stream(alternate), 880)
        self.release(alternate)
        muted = self.ready(self.plan(id, tracks, audio=None))
        self.stream(muted, audio=False)
        self.release(muted)
        print("PASS cached fractional seek, selected audio, no audio and shared cache leases")

        for name, index, top, position in [
            ("tracks.mkv", 3, False, 7.375),
            ("tracks.mkv", 3, True, 9.375),
            ("tracks.mkv", 4, False, 9.375),
            ("offset.mkv", 3, True, 9.375),
            ("bitmap.mkv", 2, False, 7.375),
            ("bitmap.mkv", 2, True, 9.375),
        ]:
            off = self.ready(self.plan(files[name], info[name], position=position), position=position)
            baseline = self.frame(self.stream(off, position=position), position)
            self.release(off)
            on = self.ready(self.plan(files[name], info[name], subtitle=index, position=position), position=position)
            burned = self.frame(self.stream(on, position=position), position)
            start, end = (0, 70 * 320) if top else (110 * 320, 180 * 320)
            pixels = sum(abs(x - y) > 50 for x, y in zip(baseline[start:end], burned[start:end]))
            assert pixels > 80, (name, index, position, "subtitle was not burned at the expected position", pixels)
            self.release(on)
        print("PASS embedded ASS, SRT, fonts, bitmap subtitles, source offsets and subtitle off")

        for change in [
            {"position": -1},
            {"position": tracks["duration"]},
            {"audio_stream_index": 99},
            {"subtitle_stream_index": 99},
            {"source_version": "stale"},
            {"capabilities": {}},
            {"encoder_profile": "client-controlled"},
            {"subtitle_stream_index": -1},
            {"variant_key": "a" * 32},
        ]:
            self.call(
                f"/api/files/{id}/playback",
                {
                    "audio_stream_index": 1,
                    **change,
                },
                "POST",
                status=400,
            )
        for route in ["/api/playback", "/api/files/" + id + "/subtitles/3", "/api/fonts/fallback", "/media/direct.mp4"]:
            self.call(route, status=404)
        self.call("/api/playback/cache", {}, "POST", status=405)
        self.call(f"/api/files/{id}/playback-plan", {}, "POST", status=405)
        self.call(f"/api/files/{id}/progress", {"position": 12.5}, "PUT", status=204)
        assert self.call(f"/api/files/{id}/progress")["watch_pos"] == 12.5
        default = self.register(id, self.call(f"/api/files/{id}/playback", method="POST"))
        assert default["position"] == 12.5, default
        assert default["selection"]["audio"] == next(
            (track["stream_index"] for track in tracks["audio_tracks"] if track["is_default"]),
            tracks["audio_tracks"][0]["stream_index"],
        )
        assert default["selection"]["subtitle"] == next(
            (track["stream_index"] for track in tracks["subtitle_tracks"] if track["is_default"] or track["is_forced"]), None
        )
        default = self.ready(default, position=12.5)
        self.release(default)
        self.call(f"/api/files/{id}/progress", {"position": -1}, "PUT", status=400)
        invalid = files["invalid.mp4"]
        self.call(f"/api/files/{invalid}/streams", status=422)
        assert "cached probe failure" in self.call(f"/api/files/{invalid}/streams", status=422)["error"]
        self.call(f"/api/files/{invalid}/probe", {}, "POST", status=422)
        print("PASS file-ID defaults, database progress, explicit selection validation and probe errors")

        durable = self.ready(self.plan(id, tracks), complete=True)
        if self.container:
            stamp = self.command("stat", "-c", "%i:%Y:%s", self.cache_path(durable) + "/segment_000000.m4s")
            self.command("sh", "-c", 'test -z "$(find /cache/subtitles /cache/fonts -mindepth 1 -print)"')
        self.release(durable)
        if self.container:
            long_id, long_info = files["long.mkv"], info["long.mkv"]
            self.active_lifecycle(long_id, long_info)
            self.continuity(files, info)
            for seconds in [40, 0]:
                interrupted = self.ready(self.plan(long_id, long_info, subtitle=4))
                assert not interrupted["complete"], "interruption fixture finished too quickly"
                prefix = self.snapshot(interrupted)
                self.restart(seconds)
                self.sessions.remove(interrupted["session_id"])
                self.call(f"/api/playback/{interrupted['session_id']}/status", status=404)
                self.assert_prefix(interrupted, prefix)
                assert not self.exists(self.cache_path(interrupted) + "/complete")
                self.command("sh", "-c", 'test -z "$(find /cache/subtitles /cache/fonts -mindepth 1 -print)"')
            print("PASS graceful and forced shutdown retain verified prefixes and remove temporary assets")

            abandoned = "0123456789abcdef" * 2
            self.command("mkdir", f"/cache/playback/{abandoned}")
            self.command("touch", f"/cache/subtitles/{abandoned}.ass")
            self.command("mkdir", f"/cache/fonts/{abandoned}")
            self.command("touch", "/cache/subtitles/keep.txt")
            invalid_cache = f"/cache/playback/{'f' * 32}"
            self.command("mkdir", invalid_cache)
            self.command("touch", invalid_cache + "/complete")
            corrupt = self.ready(self.plan(id, tracks, audio=None), complete=True)
            self.release(corrupt)
            prefix = self.snapshot(corrupt)
            last = self.segment_names(corrupt)[-1]
            del prefix[last]
            self.command("truncate", "-s", "16", self.cache_path(corrupt) + "/" + last)
            self.restart()
            assert not self.exists(f"/cache/playback/{abandoned}/complete")
            assert not self.exists(invalid_cache + "/complete")
            assert not self.exists(self.cache_path(corrupt) + "/" + last)
            assert not self.exists(self.cache_path(corrupt) + "/complete")
            self.assert_prefix(corrupt, prefix)
            assert not self.exists(f"/cache/subtitles/{abandoned}.ass")
            assert not self.exists(f"/cache/fonts/{abandoned}")
            assert self.exists("/cache/subtitles/keep.txt")
            self.command("rm", "/cache/subtitles/keep.txt")
            assert self.call(f"/api/files/{id}/progress")["watch_pos"] == 12.5
            assert "cached probe failure" not in self.call(f"/api/files/{invalid}/streams", status=422)["error"]
            reused = self.ready(self.plan(id, tracks, position=9.375), complete=True)
            assert not any(worker["directory"].startswith(self.cache_path(reused) + "/") for worker in self.workers())
            assert stamp == self.command("stat", "-c", "%i:%Y:%s", self.cache_path(reused) + "/segment_000000.m4s")
            self.stream(reused, position=9.375)
            self.release(reused)
            print("PASS complete reuse without encoding, corrupt tail repair, progress and startup cleanup")

            expired = self.ready(self.plan(id, tracks), complete=True)
            time.sleep(self.idle_seconds + 6)
            self.call(f"/api/playback/{expired['session_id']}/status", status=404)
            self.sessions.remove(expired["session_id"])
            assert self.exists(self.cache_path(expired) + "/complete")
            print("PASS heartbeat expiry removes the session without deleting the completed cache")

    def active_lifecycle(self, file, info):
        first = self.ready(self.plan(file, info, subtitle=3))
        shared = self.plan(file, info, subtitle=3, position=200.375)
        assert self.keys[first["session_id"]] == self.keys[shared["session_id"]]
        worker = self.worker(shared)
        self.release(first)
        assert self.worker(shared)["pid"] == worker["pid"]
        self.signal(worker["pid"], "STOP")
        self.drain_staged(shared, worker)
        prefix = self.snapshot(shared)
        count = len(self.segment_names(shared))
        self.release(shared)
        self.signal(worker["pid"], "CONT")
        self.wait_idle(shared)
        assert len(self.segment_names(shared)) == count + 1, "last release did not stop at the next complete segment"
        assert "#EXT-X-ENDLIST" not in self.playlist(shared)
        assert not self.exists(self.cache_path(shared) + "/complete")
        self.assert_prefix(shared, prefix)
        print("PASS one shared encoder, prompt DELETE, exactly one final segment and retained prefix")

        covered = self.plan(file, info, subtitle=3, position=0)
        assert self.worker(covered)["threads"] == 2
        state = self.call(f"/api/playback/{covered['session_id']}/target", {"position": info["duration"] - 1}, "POST")
        assert state["encoding_threads"] == 2, "seeking hot-retuned a running encoder"
        self.release(covered)
        self.wait_idle(covered)

        catchup = self.plan(file, info, subtitle=3, position=info["duration"] - 1)
        peak = self.worker(catchup)
        assert peak["threads"] == 8
        self.signal(peak["pid"], "STOP")
        active = [catchup]
        if self.limit >= 3:
            other = self.plan(file, info, audio=2, subtitle=3, position=info["duration"] - 1)
            second = self.worker(other)
            assert second["threads"] == min(8, self.thread_budget - 8), second
            self.signal(second["pid"], "STOP")
            active.append(other)
        queued = self.plan(file, info, audio=None, subtitle=3, position=info["duration"] - 1)
        state = self.call(f"/api/playback/{queued['session_id']}/status")
        assert state["generating"] and state["encoding_threads"] == 0, state
        assert sum(worker["threads"] for worker in self.workers()) <= self.thread_budget
        assert len(self.workers()) <= self.limit - 1
        self.release(queued, expect_cache=False)
        self.wait_idle(queued)
        assert not any(worker["directory"].startswith(self.cache_path(queued) + "/") for worker in self.workers())
        self.signal(peak["pid"], "CONT")
        self.release(catchup)
        self.wait_idle(catchup)
        if len(active) > 1:
            assert self.worker(other)["threads"] == second["threads"], "a running encoder grabbed newly freed threads"
            self.signal(second["pid"], "CONT")
            self.release(other)
            self.wait_idle(other)
        print("PASS startup-only thread allocation, weighted global budget and immediate queue cancellation")

        leaving = self.ready(self.plan(file, info, subtitle=3))
        worker = self.worker(leaving)
        self.signal(worker["pid"], "STOP")
        self.drain_staged(leaving, worker)
        self.release(leaving)
        returning = self.plan(file, info, subtitle=3)
        assert self.worker(returning)["pid"] == worker["pid"], "a viable stopping encoder was not reused"
        assert not self.call(f"/api/playback/{returning['session_id']}/status")["stopping"]
        self.signal(worker["pid"], "CONT")
        self.release(returning)
        self.wait_idle(returning)
        print("PASS a new same-key lease revokes a pending boundary stop")

        stalled = self.ready(self.plan(file, info, subtitle=3))
        worker = self.worker(stalled)
        self.signal(worker["pid"], "STOP")
        self.drain_staged(stalled, worker)
        prefix = self.snapshot(stalled)
        count = len(self.segment_names(stalled))
        started = time.monotonic()
        self.release(stalled)
        self.wait_idle(stalled, 36)
        elapsed = time.monotonic() - started
        self.suspended.discard(worker["pid"])
        assert 29 <= elapsed <= 35, ("30-second stop deadline was not respected", elapsed)
        assert len(self.segment_names(stalled)) == count, "a stalled incomplete tail was published"
        self.assert_prefix(stalled, prefix)
        print("PASS the 30-second forced-stop deadline preserves every committed segment")

        failed = self.ready(self.plan(file, info, subtitle=3))
        worker = self.worker(failed)
        prefix = self.snapshot(failed)
        self.signal(worker["pid"], "KILL")
        deadline = time.monotonic() + 10
        while True:
            state = self.call(f"/api/playback/{failed['session_id']}/status")
            if state["error"] and not state["generating"]:
                break
            assert time.monotonic() < deadline, "encoder failure was not reported"
            time.sleep(0.1)
        assert state["ready"] and not state["complete"]
        self.stream(state)
        self.assert_prefix(failed, prefix)
        assert not any(value["directory"].startswith(self.cache_path(failed) + "/") for value in self.workers())
        retry = self.plan(file, info, subtitle=3)
        assert self.worker(retry)["pid"] != worker["pid"]
        self.assert_prefix(retry, prefix)
        self.release(failed)
        self.release(retry)
        self.wait_idle(retry)
        print("PASS explicit generation errors, readable old ranges and manual same-key retry")

    def cached_bytes(self, plan):
        path = self.cache_path(plan)
        return self.command("cat", path + "/init.mp4", *[path + "/" + name for name in self.segment_names(plan)])

    def compare_media(self, reference, actual, audio):
        # 校验所有样本时间戳和视频帧, 避免只看几个截图漏掉边界的重复帧.
        packets = [json.loads(self.command("ffprobe", "-v", "error", "-show_entries",
            "packet=stream_index,pts_time", "-of", "json", "-i", "pipe:0", data=data))["packets"]
            for data in [reference, actual]]
        for track in range(2 if audio else 1):
            streams = [[packet for packet in values if packet["stream_index"] == track] for values in packets]
            assert len(streams[0]) == len(streams[1]), ("sample count changed", track, [len(stream) for stream in streams])
            delta = max(abs(float(a["pts_time"]) - float(b["pts_time"])) for a, b in zip(*streams))
            assert delta < 0.001, ("timestamp rounding error accumulated", track, delta)
        frames = [self.command("ffmpeg", "-v", "error", "-i", "pipe:0", "-map", "0:v",
            "-vf", "scale=64:36,format=gray", "-fps_mode", "passthrough", "-f", "rawvideo", "pipe:1", data=data)
            for data in [reference, actual]]
        assert len(frames[0]) == len(frames[1]), "decoded video frame count changed"
        worst = max(sum(abs(a - b) for a, b in zip(frames[0][i:i+2304], frames[1][i:i+2304])) / 2304
            for i in range(0, len(frames[0]), 2304))
        assert worst < 4, ("a source frame or subtitle was lost at a continuation boundary", worst)
        if audio:
            sounds = [array.array("f", self.command("ffmpeg", "-v", "error", "-i", "pipe:0",
                "-map", "0:a", "-ac", "1", "-ar", "48000", "-f", "f32le", "pipe:1", data=data))
                for data in [reference, actual]]
            assert len(sounds[0]) == len(sounds[1]), "decoded AAC sample count changed"
            # 分窗口比较波形, 不能让整段平均误差掩盖短暂静音或重复音频.
            for start in range(4800, len(sounds[0]) - 4800, 4800):
                expected, found = sounds[0][start:start+4800], sounds[1][start:start+4800]
                energy = sum(sample * sample for sample in expected)
                error = sum((a - b) ** 2 for a, b in zip(expected, found))
                assert error <= max(0.001, energy * 0.1), ("AAC waveform changed", start / 48000, error, energy)

    def continuity(self, files, info):
        for name, audio, subtitle in [
            ("resume-tracks.mkv", 1, 3), ("resume-offset.mkv", 1, 3),
            ("resume-bitmap.mkv", 1, 2), ("resume-chirp.mkv", 1, None),
            ("resume-vfr.mkv", 1, None), ("resume-tracks.mkv", None, None),
        ]:
            assert not self.sessions
            baseline = self.ready(self.plan(files[name], info[name], audio=audio, subtitle=subtitle), complete=True)
            reference = self.cached_bytes(baseline)
            self.release(baseline)
            self.wait_idle(baseline)
            # 删除的是已确认无租约的隔离测试变体, 用相同配置重新验证首次生成和多次续接.
            self.command("rm", "-r", self.cache_path(baseline))
            current = self.ready(self.plan(files[name], info[name], audio=audio, subtitle=subtitle))
            self.release(current)
            self.wait_idle(current)
            assert not self.exists(self.cache_path(current) + "/complete"), "continuation fixture completed too early"
            for _ in range(3):
                prefix = self.snapshot(current)
                previous = sum(float(line[8:].split(",")[0]) for line in self.playlist(current).splitlines()
                    if line.startswith("#EXTINF:"))
                current = self.plan(files[name], info[name], audio=audio, subtitle=subtitle, position=previous + 0.01)
                current = self.ready(current, position=previous + 0.01)
                self.release(current)
                self.wait_idle(current)
                # 先停止再逐文件比对, 避免检查耗时让短样本提前生成到 EOF.
                self.assert_prefix(current, prefix)
            current = self.ready(self.plan(files[name], info[name], audio=audio, subtitle=subtitle), complete=True)
            self.compare_media(reference, self.cached_bytes(current), audio is not None)
            self.release(current)
            self.wait_idle(current)
            print("PASS repeated continuation, exact sample counts, timestamps, waveform and frame sequence:", name, audio, subtitle)

    def eviction(self):
        assert self.container, "--eviction-only requires an isolated container with cache_max_bytes=1"
        self.call("/api/scan", {}, "POST")
        file = next(file for file in self.call("/api/files") if file["name"] == "tracks.mkv")
        id = file["pre16mib_md5"]
        info = self.call(f"/api/files/{id}/streams")
        plan = self.ready(self.plan(id, info, subtitle=3), complete=True)
        path = self.cache_path(plan)
        assert self.exists(path + "/complete"), "active cache was evicted"
        self.stream(plan)
        self.call(f"/api/playback/{plan['session_id']}", method="DELETE", status=204)
        self.sessions.remove(plan["session_id"])
        deadline = time.monotonic() + 10
        while self.exists(path) and time.monotonic() < deadline:
            time.sleep(0.1)
        assert not self.exists(path), "unleased oversize cache was not evicted"
        partial_file = next(file for file in self.call("/api/files") if file["name"] == "long.mkv")
        partial_info = self.call(f"/api/files/{partial_file['pre16mib_md5']}/streams")
        partial = self.ready(self.plan(partial_file["pre16mib_md5"], partial_info))
        assert not partial["complete"]
        self.release(partial, expect_cache=False)
        deadline = time.monotonic() + 40
        while self.exists(self.cache_path(partial)) and time.monotonic() < deadline:
            time.sleep(0.1)
        assert not self.exists(self.cache_path(partial)), "inactive partial cache was not evicted"
        self.command("sh", "-c", 'test -z "$(find /cache/subtitles /cache/fonts -mindepth 1 -print)"')
        print("PASS real byte limit overshoot while leased and cleanup after release")

    def close(self):
        for pid in list(self.suspended):
            if any(worker["pid"] == pid for worker in self.workers()):
                self.signal(pid, "CONT")
        for id in self.sessions[:]:
            self.call(f"/api/playback/{id}", method="DELETE", status=204)
            self.sessions.remove(id)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--url")
    parser.add_argument("--fixtures", type=Path)
    parser.add_argument("--font", type=Path)
    parser.add_argument("--container", help="Isolated test container; enables process checks and restarts it")
    parser.add_argument("--media-dir", type=Path, default=Path("/media"))
    parser.add_argument("--max-processes", type=int, default=2)
    parser.add_argument("--thread-budget", type=int, default=16)
    parser.add_argument("--idle-timeout", type=int, default=60)
    parser.add_argument("--eviction-only", action="store_true", help="Requires cache_max_bytes=1 on an isolated container")
    args = parser.parse_args()
    if args.fixtures:
        assert args.font and args.font.is_file(), "--font must be an existing test font"
        fixtures(args.fixtures, args.font)
    if args.url:
        client = Client(args.url, args.container, args.media_dir, args.max_processes, args.thread_budget, args.idle_timeout)
        try:
            if args.eviction_only:
                client.eviction()
            else:
                client.run()
        finally:
            client.close()
