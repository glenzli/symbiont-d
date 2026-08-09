export function initVoiceInput({
  state,
  input,
  button,
  status,
  statusLabel,
  waveform,
  elapsed,
  notify,
  setPersistentStatus,
  resize,
}) {
  let recorder = null;
  let stream = null;
  let chunks = [];
  let discardRecording = false;
  let transcriptionController = null;
  let audioContext = null;
  let analyser = null;
  let waveformFrame = null;
  let elapsedTimer = null;
  let recordingStartedAt = 0;
  let waveformSource = null;
  let lastWaveformSampleAt = 0;
  const waveformRenderer = createWaveformRenderer(waveform);

  function configuration() {
    return state.audioTranscription || {};
  }

  function setState(next) {
    button.dataset.state = next;
    const recording = next === "recording";
    const transcribing = next === "transcribing";
    button.classList.toggle("recording", recording);
    button.classList.toggle("transcribing", transcribing);
    button.disabled = false;
    button.title = recording
      ? "停止录音并转写"
      : transcribing
        ? "取消转写"
        : "语音输入";
    button.setAttribute("aria-label", button.title);
    button.dataset.tooltip = button.title;
    updateStatus(next);
  }

  function updateStatus(next) {
    if (!status) return;
    const recording = next === "recording";
    const transcribing = next === "transcribing";
    status.hidden = !recording && !transcribing;
    status.dataset.state = next;
    if (statusLabel) statusLabel.textContent = recording ? "正在录音" : "正在本地转写";
    if (elapsed) elapsed.hidden = !recording;
    if (recording) window.requestAnimationFrame(() => waveformRenderer.resize());
    if (recording) setPersistentStatus?.("正在录音 · 点击方块结束");
    if (transcribing) setPersistentStatus?.("正在本地转写");
  }

  async function startRecording() {
    const config = configuration();
    if (!config.enabled) {
      notify("请先在设置 → 连接中启用本地语音转写");
      return;
    }
    if (!navigator.mediaDevices?.getUserMedia || !window.MediaRecorder) {
      notify("当前页面环境不支持麦克风录音");
      return;
    }
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const mimeType = preferredMimeType();
      recorder = mimeType
        ? new MediaRecorder(stream, { mimeType })
        : new MediaRecorder(stream);
      chunks = [];
      discardRecording = false;
      recorder.addEventListener("dataavailable", (event) => {
        if (event.data.size) chunks.push(event.data);
      });
      recorder.addEventListener("stop", finishRecording, { once: true });
      recorder.start();
      beginRecordingMeter(stream);
      setState("recording");
    } catch (error) {
      releaseStream();
      setState("idle");
      notify(microphoneError(error));
    }
  }

  function stopRecording() {
    if (recorder?.state === "recording") recorder.stop();
  }

  async function finishRecording() {
    const activeRecorder = recorder;
    recorder = null;
    releaseStream();
    if (discardRecording || !chunks.length) {
      chunks = [];
      setState("idle");
      notify("已取消录音");
      return;
    }
    const mimeType = activeRecorder?.mimeType || chunks[0]?.type || "audio/webm";
    const blob = new Blob(chunks, { type: mimeType });
    chunks = [];
    if (!blob.size) {
      setState("idle");
      notify("没有录到声音，请重试");
      return;
    }
    await transcribe(blob, mimeType);
  }

  async function transcribe(blob, mimeType) {
    transcriptionController = new AbortController();
    setState("transcribing");
    try {
      const body = new FormData();
      body.append("audio", blob, recordingFilename(mimeType));
      const response = await fetch("/api/voice/transcriptions", {
        method: "POST",
        body,
        signal: transcriptionController.signal,
      });
      const payload = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(payload.error || "本地转写失败");
      insertTranscript(payload.text || "");
      notify("已转写到输入框，可修改后发送");
    } catch (error) {
      if (error.name === "AbortError") notify("已取消转写");
      else notify(error.message || "本地转写失败");
    } finally {
      transcriptionController = null;
      setState("idle");
    }
  }

  function cancel() {
    if (recorder?.state === "recording") {
      discardRecording = true;
      stopRecording();
      return;
    }
    transcriptionController?.abort();
  }

  function insertTranscript(text) {
    const value = String(text).trim();
    if (!value) throw new Error("本地转写没有返回可用文本");
    const start = input.selectionStart ?? input.value.length;
    const end = input.selectionEnd ?? start;
    const prefix = input.value.slice(0, start);
    const suffix = input.value.slice(end);
    const needsSpace = prefix.trim() && !/\s$/.test(prefix) ? " " : "";
    input.value = `${prefix}${needsSpace}${value}${suffix}`;
    const caret = prefix.length + needsSpace.length + value.length;
    input.setSelectionRange(caret, caret);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.focus();
    resize();
  }

  function releaseStream() {
    stream?.getTracks().forEach((track) => track.stop());
    stream = null;
    stopRecordingMeter();
  }

  button.addEventListener("click", () => {
    if (recorder?.state === "recording") stopRecording();
    else if (transcriptionController) cancel();
    else startRecording();
  });
  window.addEventListener("pagehide", cancel);
  setState("idle");
  return { configUpdated: () => {} };

  function beginRecordingMeter(activeStream) {
    recordingStartedAt = Date.now();
    lastWaveformSampleAt = 0;
    updateElapsed();
    elapsedTimer = window.setInterval(updateElapsed, 250);
    const AudioContext = window.AudioContext || window.webkitAudioContext;
    if (!AudioContext || !activeStream) return;
    try {
      audioContext = new AudioContext();
      analyser = audioContext.createAnalyser();
      analyser.fftSize = 64;
      waveformSource = audioContext.createMediaStreamSource(activeStream);
      waveformSource.connect(analyser);
      audioContext.resume().catch(() => {});
      waveformRenderer.clear();
      renderWaveform();
    } catch {
      waveformRenderer.clear();
    }
  }

  function stopRecordingMeter() {
    if (waveformFrame !== null) window.cancelAnimationFrame(waveformFrame);
    waveformFrame = null;
    if (elapsedTimer !== null) window.clearInterval(elapsedTimer);
    elapsedTimer = null;
    analyser = null;
    waveformSource?.disconnect();
    waveformSource = null;
    if (audioContext) audioContext.close().catch(() => {});
    audioContext = null;
  }

  function renderWaveform() {
    if (!analyser) return;
    const now = performance.now();
    if (now - lastWaveformSampleAt >= 32) {
      const samples = new Uint8Array(analyser.fftSize);
      analyser.getByteTimeDomainData(samples);
      const variance = samples.reduce((sum, sample) => {
        const value = (sample - 128) / 128;
        return sum + value * value;
      }, 0) / samples.length;
      waveformRenderer.push(Math.min(1, Math.sqrt(variance) * 5.5));
      lastWaveformSampleAt = now;
    }
    waveformFrame = window.requestAnimationFrame(renderWaveform);
  }

  function updateElapsed() {
    if (!elapsed) return;
    const seconds = Math.floor((Date.now() - recordingStartedAt) / 1000);
    elapsed.textContent = `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
  }

}

function createWaveformRenderer(canvas) {
  const history = [];
  const context = canvas?.getContext("2d");
  let width = 0;
  let height = 0;
  let pixelRatio = 1;

  function resize() {
    if (!canvas || !context) return;
    const bounds = canvas.getBoundingClientRect();
    pixelRatio = Math.max(1, window.devicePixelRatio || 1);
    width = Math.floor(bounds.width);
    height = Math.floor(bounds.height);
    canvas.width = Math.max(1, Math.floor(width * pixelRatio));
    canvas.height = Math.max(1, Math.floor(height * pixelRatio));
    context.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);
    trimHistory();
    draw();
  }

  function push(level) {
    history.push(Math.max(0.035, level));
    trimHistory();
    draw();
  }

  function clear() {
    history.length = 0;
    resize();
  }

  function trimHistory() {
    const capacity = Math.max(1, Math.ceil(width / 3.2));
    if (history.length > capacity) history.splice(0, history.length - capacity);
  }

  function draw() {
    if (!context || !width || !height) return;
    context.clearRect(0, 0, width, height);
    context.strokeStyle = "#222426";
    context.lineWidth = 2.1;
    context.lineCap = "round";
    const baseline = height / 2;
    const spacing = 3.2;
    const startX = Math.max(1, width - history.length * spacing);
    history.forEach((level, index) => {
      const amplitude = 1.5 + level * (height / 2 - 1.5);
      const x = startX + index * spacing;
      context.beginPath();
      context.moveTo(x, baseline - amplitude);
      context.lineTo(x, baseline + amplitude);
      context.stroke();
    });
  }

  if (canvas && "ResizeObserver" in window) {
    new ResizeObserver(resize).observe(canvas);
  }
  return { resize, push, clear };
}

function preferredMimeType() {
  const types = ["audio/webm;codecs=opus", "audio/mp4", "audio/webm"];
  return types.find((type) => MediaRecorder.isTypeSupported(type)) || "";
}

function recordingFilename(mimeType) {
  const extension = mimeType.includes("mp4") ? "m4a" : "webm";
  return `voice-${Date.now()}.${extension}`;
}

function microphoneError(error) {
  if (error?.name === "NotAllowedError") return "需要允许麦克风权限才能录音";
  if (error?.name === "NotFoundError") return "没有找到可用麦克风";
  return "无法开始录音，请检查麦克风后重试";
}
