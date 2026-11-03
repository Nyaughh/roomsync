package dev.roomsync;

/** Bounded startup-only effective-buffer tuning. Capacity is not a DAC timestamp. */
final class QueueTuner {
    static final int BLOCK_FRAMES=480;
    final int capacity;
    private int frames,adjustments;
    private boolean sealed;
    QueueTuner(int capacity,int actualFrames){
        if(capacity<1||actualFrames<1||actualFrames>capacity)throw new IllegalStateException("AudioTrack reported an invalid buffer size");
        this.capacity=capacity;frames=actualFrames;
    }
    static int initialRequest(int capacity){if(capacity<1)throw new IllegalStateException("AudioTrack buffer capacity is unavailable");return Math.min(capacity,BLOCK_FRAMES*2);}
    int nextRequest(){
        if(sealed)throw new IllegalStateException("Audio output underrun. Reconnect to recalibrate.");
        if(frames>=capacity)throw new IllegalStateException("This output could not stabilize within its audio buffer. Try the phone speaker or a wired output.");
        return (int)Math.min((long)capacity,(long)frames+BLOCK_FRAMES);
    }
    void acceptGrowth(int actual){
        if(sealed||actual<=frames||actual>capacity)throw new IllegalStateException("Audio output buffer could not grow safely. Reconnect with a different output.");
        frames=actual;adjustments++;
    }
    void seal(){sealed=true;}
    int frames(){return frames;}
    int adjustments(){return adjustments;}
}
