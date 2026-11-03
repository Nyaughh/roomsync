# Windows speaker + Android playback

This preview can play the same captured PCM through a selected Windows output
and Android receivers on one future presentation timeline. It requires a silent
virtual source route. It does not delay audio that is still playing directly
through the physical speakers, install a driver, or change Windows settings.
Physical speaker skew and end-to-end acoustic latency remain unmeasured.

## Set up the route yourself

1. If needed, obtain VB-CABLE only from [VB-Audio's official page](https://vb-audio.com/Cable/)
   and follow its installation instructions. The vendor requires administrator
   installation and a restart. Check the publisher and any resulting default
   playback/recording-device changes. RoomSync does not install or bundle it.
2. Send the source application's playback, or the Windows output you intend to
   share, to **CABLE Input**. This is the virtual playback endpoint. Start RoomSync
   again after installing the driver so the endpoint list is current.
3. In RoomSync's **Advanced settings**, enable **Play through this PC too**.
   Choose the explicit **CABLE Input** under **Virtual capture output**. The
   recording endpoint called **CABLE Output** is not the source selector used by
   this loopback implementation. The vendor documents playback-loopback support
   enabled by default in its current driver.
4. Choose the actual endpoint under **Physical speakers**. It must differ from
   the capture endpoint. Do not choose a virtual cable as the destination.
5. Keep Windows **Listen to this device** and other mixer/monitor paths off for
   this route. They can duplicate undelayed audio or create feedback. Confirm the
   routing checklist, create a setup code, then connect Android as usual.
6. Start at a low listening volume. Both outputs stay silent during calibration.
   RoomSync then chooses a common lead covering the slower output queue and starts
   one shared timeline. Connection details show local timing and concealment.

The input/output naming and loopback behavior are described in the
[VB-CABLE reference manual](https://vb-audio.com/Cable/VBCABLE_ReferenceManual.pdf),
particularly pages 3 and 13. RoomSync uses
[WASAPI render-endpoint loopback](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording),
not microphone capture.

**After Stop:** RoomSync stops its streams but leaves your chosen Windows/app
routing unchanged. Audio still sent to the virtual cable may therefore be silent.
Select your previous physical output yourself when you want ordinary playback
again. RoomSync does not restore, mute or change a system setting behind your back.

## What is guarded

- Capture and output must be explicitly selected, listed and distinct. The native
  adapter also checks the resolved endpoint IDs before priming or starting output.
- Endpoint IDs remain pinned for the session. There is no automatic default-device
  fallback. Unplug, invalid timing or a lost output cursor requires restarting.
- Local output first plays silence while at least 500 ms of advancing hardware
  clock observations establish its queue requirement. The same render stream
  continues through playback, using host QPC time and the phones' packet epoch.
- One shared delay covers local and remote output needs, capture allowance and
  the 30 ms network/clock reserve. It remains capped at 300 ms; a local speaker can
  increase the group delay if it needs more buffering.
- The callback uses fixed-capacity packet storage, a prebuilt renderer and atomics.
  It does not allocate, take a mutex, log or perform network I/O.

This preview conservatively requires the physical output's actual shared buffer
to span at least two reported engine periods. Smaller/unknown buffers fail with
an explicit unsupported-output error. An empty queue after priming, inaccurate
clock, missed render deadline or increased queue requirement stops the group
instead of guessing a new presentation origin. This is a compatibility restriction
of this preview, not a requirement imposed by Windows or proof of an audible gap.
External mixer/monitor routing cannot be inferred from endpoint IDs; the routing
confirmation is still necessary.

## Verification and limits

Portable tests exercise actual capture packetization, the local callback and a
remote renderer with different output queues. They compare nonconstant PCM at
the same virtual DAC times, plus invalid clocks, queue bounds and feedback-route
validation. Windows cross-compilation establishes buildability only.

Before treating this as synchronized room audio, test the intended physical
speakers/phones with a common impulse recording, including long playback, CPU and
Wi-Fi load, route changes, unplug/replug and Stop/Restart. Speaker DSP, Bluetooth,
clock-path asymmetry and acoustic distance can add differences not measured here.
