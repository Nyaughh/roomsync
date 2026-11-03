package dev.roomsync;
public final class FocusStateTest {
    static void check(boolean b){if(!b)throw new AssertionError();}
    public static void main(String[] args){
        FocusState state=new FocusState();check(state.volume()==1f);state.duck();check(state.volume()==0.2f);check(!state.expired(100_000_000_000L));
        state.transientLoss(1);check(state.volume()==0f);check(!state.expired(29_000_000_000L));state.transientLoss(20_000_000_000L);check(state.timeoutDelayMs(20_000_000_001L)==10_000L);check(state.expired(30_000_000_001L));
        state.gain();check(state.volume()==1f&&!state.expired(100_000_000_000L));state.transientLoss(100_000_000_000L);state.duck();check(state.volume()==0.2f&&!state.expired(150_000_000_000L));
        System.out.println("FocusStateTest: duck, bounded transient mute, repeated loss, and gain passed");
    }
}
