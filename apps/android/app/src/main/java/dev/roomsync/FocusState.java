package dev.roomsync;

/** Keep the device clock running during brief focus interruptions; never resume queued stale audio. */
final class FocusState {
    private volatile float volume=1f;
    private volatile long mutedSince=-1;
    void gain(){mutedSince=-1;volume=1f;}
    void duck(){mutedSince=-1;volume=0.2f;}
    void transientLoss(long now){if(mutedSince<0)mutedSince=now;volume=0f;}
    float volume(){return volume;}
    long timeoutDelayMs(long now){long since=mutedSince;return since<0?0:Math.max(0,(30_000_000_000L-(now-since)+999_999L)/1_000_000L);}
    boolean expired(long now){long since=mutedSince;return since>=0&&now-since>=30_000_000_000L;}
    String describe(){float current=volume;return current==0f?"temporarily muted; keeping live timing (up to 30 seconds)":current<1f?"temporarily lowered for another app":"normal";}
}
