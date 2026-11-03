package dev.roomsync;
public final class OutputBudgetTest {
    static void check(boolean value){if(!value)throw new AssertionError();}
    public static void main(String[] args){
        long base=1_000_000_000L;OutputBudget b=new OutputBudget();
        for(int i=0;i<50;i++){long now=base+i*10_000_000L;check(!b.observe(now+150_000_000L,now,i,now));}
        check(!b.ready());long now=base+500_000_000L;check(b.observe(now+140_000_000L,now,50,now));check(b.horizonNs()==150_000_000L);
        // One timing outlier is visible but does not falsely kill an otherwise stable route.
        check(!b.observe(now+170_000_000L,now,51,now));now+=10_000_000L;check(!b.observe(now+150_000_000L,now,52,now));
        boolean threw=false;for(int i=0;i<=10;i++){long t=now+i*10_000_000L;try{b.observe(t+170_000_000L,t,53+i,t);}catch(IllegalStateException e){threw=true;}}check(threw);
        OutputBudget duplicate=new OutputBudget();for(int i=0;i<100;i++){long t=base+i*10_000_000L;check(!duplicate.observe(t+150_000_000L,t,1,base));}check(!duplicate.ready());
        OutputBudget tooFew=new OutputBudget();check(!tooFew.observe(base+150_000_000L,base,1,base));check(!tooFew.observe(base+750_000_000L,base+600_000_000L,2,base+600_000_000L));
        OutputBudget growing=new OutputBudget();for(int i=0;i<=50;i++){long t=base+i*10_000_000L;growing.observe(t+40_000_000L+i*2_000_000L,t,i,t);}check(growing.horizonNs()==140_000_000L);
        System.out.println("OutputBudgetTest: measured maximum, steady window, advancing observations, and live latency guard passed");
    }
}
