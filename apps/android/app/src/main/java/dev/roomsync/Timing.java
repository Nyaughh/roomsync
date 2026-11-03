package dev.roomsync;

/** Pure projection logic, also tested on the host JVM. Never substitutes wall-clock time. */
final class Timing {
    private long lastPosition = -1;
    private long lastNs = -1;
    private boolean advancing;
    private long advances;
    private long invalidSince = -1;

    boolean observe(long position, long nanoTime, long nowNs, long written) {
        // AudioTrack only guarantees the low 32 bits of framePosition. Extend
        // forward wraps, while retaining rejection of ordinary backwards jumps.
        if (position >= 0 && position <= 0xffffffffL && lastPosition >= 0) {
            position += lastPosition & ~0xffffffffL;
            if (position < lastPosition - 0x80000000L) position += 0x100000000L;
        }
        if (position < 0 || position > written || nanoTime <= 0 || nanoTime > nowNs + 100_000_000L
                || nowNs - nanoTime > 250_000_000L) return false;
        if (lastPosition >= 0 && (position < lastPosition || nanoTime < lastNs)) {
            throw new IllegalStateException("Audio hardware timeline changed. Reconnect to recalibrate.");
        }
        if (lastPosition >= 0 && position > lastPosition && nanoTime > lastNs) { advancing = true; advances++; }
        lastPosition = position;
        lastNs = nanoTime;
        return advancing;
    }

    long project(long written, long nowNs) {
        if (!advancing || lastPosition < 0 || nowNs - lastNs > 250_000_000L) return -1;
        long ahead = written - lastPosition;
        if (ahead < 0 || ahead > 48_000) throw new IllegalStateException("Audio output queue left its safe bounds");
        long result = lastNs + ahead * 1_000_000_000L / 48_000L;
        // A past deadline is an underrun, not permission to silently play late.
        if (result < nowNs - 5_000_000L) return -1;
        invalidSince = -1;
        return result;
    }

    long advances() { return advances; }
    long observationNs() { return lastNs; }

    boolean expired(long nowNs) {
        if (invalidSince < 0) invalidSince = nowNs;
        return nowNs - invalidSince > 5_000_000_000L;
    }
}
