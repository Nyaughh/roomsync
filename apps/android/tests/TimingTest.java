package dev.roomsync;
public final class TimingTest {
    static void check(boolean value){if(!value)throw new AssertionError();}
    public static void main(String[] args){
        Timing t=new Timing();long second=1_000_000_000L;
        check(!t.observe(0,second,second,480));
        check(t.observe(480,second+10_000_000L,second+10_000_000L,960));
        check(t.project(960,second+10_000_000L)==second+20_000_000L);
        check(t.project(960,second+300_000_000L)==-1);
        check(!t.observe(3000,second+20_000_000L,second+20_000_000L,960));
        boolean threw=false;try{t.observe(2,second+30_000_000L,second+30_000_000L,960);}catch(IllegalStateException e){threw=true;}check(threw);
        Timing stale=new Timing();check(!stale.expired(second));check(stale.expired(second+5_000_000_001L));
        Timing future=new Timing();check(!future.observe(0,second+200_000_000L,second,100));
        Timing stuck=new Timing();boolean timedOut=false;
        for(int i=0;i<1000;i++){long now=second+i*10_000_000L;check(!stuck.observe(0,now,now,480));if(stuck.expired(now))timedOut=true;}check(timedOut);
        Timing late=new Timing();timedOut=false;
        for(int i=0;i<1000;i++){long now=second+i*10_000_000L;late.observe(i*480,now-10_000_000L,now,i*480);check(late.project(i*480,now)==-1);if(late.expired(now))timedOut=true;}check(timedOut);
        Timing wrap=new Timing();check(!wrap.observe(0xfffffff0L,second,second,0x100000010L));check(wrap.observe(32,second+1_000_000L,second+1_000_000L,0x100000100L));check(wrap.project(0x100000100L,second+1_000_000L)>0);
        System.out.println("TimingTest: advancing clock, projection, stale/future/invalid bounds and discontinuity passed");
    }
}
