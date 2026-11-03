package dev.roomsync;
public final class QueueTunerTest {
    static void check(boolean b){if(!b)throw new AssertionError();}
    static void rejects(Runnable call){boolean threw=false;try{call.run();}catch(IllegalStateException e){threw=true;}check(threw);}
    public static void main(String[] args){
        check(QueueTuner.initialRequest(1920)==960);check(QueueTuner.initialRequest(480)==480);
        QueueTuner q=new QueueTuner(1920,960);check(q.nextRequest()==1440);q.acceptGrowth(1536);check(q.frames()==1536&&q.adjustments()==1);check(q.nextRequest()==1920);q.acceptGrowth(1920);rejects(q::nextRequest);
        QueueTuner fixed=new QueueTuner(1920,1200);rejects(()->fixed.acceptGrowth(1200));rejects(()->fixed.acceptGrowth(1921));fixed.seal();rejects(fixed::nextRequest);rejects(()->fixed.acceptGrowth(1680));
        rejects(()->new QueueTuner(0,0));rejects(()->new QueueTuner(100,200));rejects(()->QueueTuner.initialRequest(0));
        System.out.println("QueueTunerTest: requested20ms, actual-size rounding, bounded growth, refused growth and readiness freeze passed");
    }
}
