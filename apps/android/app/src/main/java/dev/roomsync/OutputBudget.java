package dev.roomsync;

/** Measured next-write horizon, not AudioTrack buffer capacity or wall-clock guesses. */
final class OutputBudget {
    static final long BLOCK_NS=10_000_000L;
    static final long STARTUP_TIMEOUT_NS=5_000_000_000L;
    private long firstNs=-1, firstObservation=-1, firstAdvance, maxHorizon, frozen=-1;
    private long excessSince=-1;

    boolean observe(long dacNs,long nowNs,long advances,long observationNs) {
        long horizon=Math.max(0,dacNs-nowNs);
        if(horizon>1_000_000_000L)throw new IllegalStateException("Audio output latency exceeds the supported bound. Try the phone speaker or wired audio.");
        if(frozen>=0){
            if(horizon>frozen+10_000_000L){
                if(excessSince<0)excessSince=nowNs;
                if(nowNs-excessSince>=100_000_000L)throw new IllegalStateException("Audio output latency increased after calibration. Reconnect to recalibrate.");
            }else excessSince=-1;
            return false;
        }
        if(firstNs<0){firstNs=nowNs;firstObservation=observationNs;firstAdvance=advances;}
        maxHorizon=Math.max(maxHorizon,horizon);
        // Duplicate/stale timestamp queries cannot make the output ready. Keep
        // writing actual silence while observing the steadily occupied queue.
        if(nowNs-firstNs>=500_000_000L&&observationNs-firstObservation>=500_000_000L&&advances-firstAdvance>=5){
            frozen=maxHorizon;
            return true;
        }
        return false;
    }
    boolean canPublish(long nowNs,long startedNs,int expectedUnderruns,int observedUnderruns){
        if(!ready())throw new IllegalStateException("Output calibration is incomplete");
        if(nowNs-startedNs>STARTUP_TIMEOUT_NS)throw new IllegalStateException("Audio output could not stabilize within five seconds. Try a different output.");
        if(observedUnderruns<expectedUnderruns)throw new IllegalStateException("Audio output timeline changed. Reconnect to recalibrate.");
        return observedUnderruns==expectedUnderruns;
    }
    boolean ready(){return frozen>=0;}
    long horizonNs(){if(!ready())throw new IllegalStateException("Output calibration is incomplete");return frozen;}
}
