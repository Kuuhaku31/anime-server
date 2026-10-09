"""Exercise shared HLS caches against an isolated server with generated media."""

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
    # A white PGS rectangle moves at 9 seconds, making seek and burn-in measurable.
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
    (directory / "direct.eng.srt").write_bytes(srt.read_bytes())
    (directory / "direct.zh.ass").write_bytes(subtitle.read_bytes())
    (directory / "invalid.mp4").write_bytes(b"intentionally invalid media for probe error tests")


class Client:
    def __init__(self, base, container, media, limit):
        self.base = base.rstrip("/")
        self.container = container
        self.media = Path("/media") if container else media
        self.limit = limit
        self.sessions = []
        self.keys = {}

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
            f"/api/files/{file}/playback-plan",
            {
                "video_stream_index": 0,
                "audio_stream_index": audio,
                "subtitle_stream_index": subtitle,
                "position": position,
            },
            "POST",
        )
        self.sessions.append(result["session_id"])
        config = {
            "src_file_pre16mib_md5": file, "video_stream": 0, "audio_stream": audio,
            "subtitle_stream": subtitle, "encoder_profile": "h264-high-yuv420p-aac-fmp4-v1",
        }
        self.keys[result["session_id"]] = hashlib.md5(json.dumps(config, separators=(",", ":")).encode()).hexdigest()
        assert set(result) == {
            "session_id",
            "url",
            "mime_type",
            "start_time",
            "duration",
            "state",
            "ready",
            "buffered_until",
            "error",
        }, result
        assert result["mime_type"] == "application/vnd.apple.mpegurl"
        assert result["start_time"] == 0, result
        return result

    def ready(self, plan, position=0, complete=False):
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            state = self.call(f"/api/playback/{plan['session_id']}/status")
            assert state["state"] not in ("failed", "cancelled"), state
            if state["ready"] and state["buffered_until"] > position and (not complete or state["state"] == "complete"):
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

    def restart(self, seconds=5):
        subprocess.run(["docker", "restart", "--time", str(seconds), self.container],
            check=True, stdout=subprocess.DEVNULL)
        deadline = time.monotonic() + 30
        while True:
            try:
                self.call("/health")
                return
            except (urllib.error.URLError, ConnectionError):
                assert time.monotonic() < deadline, "server failed to restart"
                time.sleep(0.2)

    def release(self, plan):
        id = plan["session_id"]
        self.call(f"/api/playback/{id}", method="DELETE", status=204)
        self.sessions.remove(id)
        self.call(f"/api/playback/{id}/status", status=404)
        if self.container:
            assert self.exists(self.cache_path(plan)), "releasing a lease deleted its cache"

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
        reference = self.command(
            "ffmpeg",
            "-v",
            "error",
            "-ss",
            "7.375",
            "-i",
            str(self.media / "tracks.mkv"),
            "-frames:v",
            "1",
            "-vf",
            "format=gray",
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
                f"/api/files/{id}/playback-plan",
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
        self.call(f"/api/files/{id}/playback", {}, "POST", status=405)
        self.call(f"/api/files/{id}/progress", {"position": 12.5}, "PUT", status=204)
        assert self.call(f"/api/files/{id}/progress")["watch_pos"] == 12.5
        self.call(f"/api/files/{id}/progress", {"position": -1}, "PUT", status=400)
        invalid = files["invalid.mp4"]
        self.call(f"/api/files/{invalid}/streams", status=422)
        assert "cached probe failure" in self.call(f"/api/files/{invalid}/streams", status=422)["error"]
        self.call(f"/api/files/{invalid}/probe", {}, "POST", status=422)
        print("PASS validation, removed routes, progress and process-local probe errors")

        durable = self.ready(self.plan(id, tracks), complete=True)
        if self.container:
            stamp = self.command("stat", "-c", "%i:%Y:%s", self.cache_path(durable) + "/segment_000000.m4s")
            self.command("sh", "-c", 'test -z "$(find /cache/subtitles /cache/fonts -mindepth 1 -print)"')
        self.release(durable)
        first = self.ready(self.plan(files["long.mkv"], info["long.mkv"], subtitle=3))
        shared = self.plan(files["long.mkv"], info["long.mkv"], subtitle=3, position=200.375)
        assert self.keys[first["session_id"]] == self.keys[shared["session_id"]]
        if self.container:
            output = subprocess.check_output(["docker", "top", self.container, "-eo", "pid,comm,args"], text=True)
            assert sum(line.split()[1] == "ffmpeg" and "long.mkv" in line for line in output.splitlines()[1:]) == 1
        self.release(first)
        self.stream(self.ready(shared))
        active = [shared]
        if self.limit > 1:
            active.append(self.ready(self.plan(files["long.mkv"], info["long.mkv"], audio=2)))
        queued = self.plan(files["long.mkv"], info["long.mkv"], audio=None)
        if self.limit <= 2:
            assert self.call(f"/api/playback/{queued['session_id']}/status")["state"] == "queued"
        if self.container:
            output = subprocess.check_output(["docker", "top", self.container, "-eo", "pid,comm,args"], text=True)
            assert sum(line.split()[1] == "ffmpeg" for line in output.splitlines()[1:]) <= self.limit
        active.append(queued)
        for plan in active:
            self.release(plan)
        if self.container:
            deadline = time.monotonic() + 180
            for plan in active:
                while not self.exists(self.cache_path(plan) + "/complete"):
                    assert time.monotonic() < deadline, "unleased generation did not finish"
                    time.sleep(0.5)
            self.command("sh", "-c", 'test -z "$(find /cache/subtitles /cache/fonts -mindepth 1 -print)"')
        print("PASS shared FFmpeg, process limit, queued work and background completion after lease release")

        if self.container:
            for seconds in [5, 0]:
                interrupted = self.ready(self.plan(files["long.mkv"], info["long.mkv"], subtitle=4))
                assert interrupted["state"] != "complete", "interruption fixture finished too quickly"
                self.command("sh", "-c", 'test -n "$(find /cache/subtitles /cache/fonts -mindepth 1 -print)"')
                self.restart(seconds)
                self.sessions.remove(interrupted["session_id"])
                self.call(f"/api/playback/{interrupted['session_id']}/status", status=404)
                assert not self.exists(self.cache_path(interrupted)), "interrupted cache survived recovery"
                self.command("sh", "-c", 'test -z "$(find /cache/subtitles /cache/fonts -mindepth 1 -print)"')
            print("PASS graceful shutdown and forced interruption clean active generation and temporary assets")

            abandoned = "0123456789abcdef" * 2
            self.command("mkdir", f"/cache/playback/{abandoned}")
            self.command("touch", f"/cache/subtitles/{abandoned}.ass")
            self.command("mkdir", f"/cache/fonts/{abandoned}")
            self.command("touch", "/cache/subtitles/keep.txt")
            invalid_cache = f"/cache/playback/{'f' * 32}"
            self.command("mkdir", invalid_cache)
            self.command("touch", invalid_cache + "/complete")
            self.command("rm", self.cache_path(muted) + "/segment_000000.m4s")
            self.restart()
            assert not self.exists(f"/cache/playback/{abandoned}")
            assert not self.exists(invalid_cache)
            assert not self.exists(self.cache_path(muted))
            assert not self.exists(f"/cache/subtitles/{abandoned}.ass")
            assert not self.exists(f"/cache/fonts/{abandoned}")
            assert self.exists("/cache/subtitles/keep.txt")
            self.command("rm", "/cache/subtitles/keep.txt")
            assert self.call(f"/api/files/{id}/progress")["watch_pos"] == 12.5
            assert "cached probe failure" not in self.call(f"/api/files/{invalid}/streams", status=422)["error"]
            reused = self.plan(id, tracks, position=9.375)
            assert reused["state"] == "complete" and reused["ready"]
            assert stamp == self.command("stat", "-c", "%i:%Y:%s", self.cache_path(reused) + "/segment_000000.m4s")
            self.stream(reused, position=9.375)
            self.release(reused)
            print("PASS restart reuse without encoding, corrupt cache cleanup, progress and transient asset recovery")

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
        assert not self.exists(path), "unleased oversize cache was not evicted"
        self.command("sh", "-c", 'test -z "$(find /cache/subtitles /cache/fonts -mindepth 1 -print)"')
        print("PASS real byte limit overshoot while leased and cleanup after release")

    def close(self):
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
    parser.add_argument("--eviction-only", action="store_true", help="Requires cache_max_bytes=1 on an isolated container")
    args = parser.parse_args()
    if args.fixtures:
        assert args.font and args.font.is_file(), "--font must be an existing test font"
        fixtures(args.fixtures, args.font)
    if args.url:
        client = Client(args.url, args.container, args.media_dir, args.max_processes)
        try:
            if args.eviction_only:
                client.eviction()
            else:
                client.run()
        finally:
            client.close()
