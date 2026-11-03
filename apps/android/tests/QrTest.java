package dev.roomsync;
import com.google.zxing.*;
import com.google.zxing.common.*;
import com.google.zxing.qrcode.QRCodeWriter;
import java.util.*;
public final class QrTest {
    public static void main(String[] args)throws Exception{
        String key=new String(new char[64]).replace('\0','b');String code="RS1|192.168.1.8|5000|5001|"+key;
        int width=480,height=480;BitMatrix bits=new QRCodeWriter().encode(code,BarcodeFormat.QR_CODE,width,height);
        byte[] cameraFrame=new byte[width*height*3/2];Arrays.fill(cameraFrame,(byte)128);
        for(int y=0;y<height;y++)for(int x=0;x<width;x++)cameraFrame[y*width+x]=(byte)(bits.get(x,y)?0:255);
        Map<DecodeHintType,Object> hints=new EnumMap<>(DecodeHintType.class);hints.put(DecodeHintType.POSSIBLE_FORMATS,Collections.singletonList(BarcodeFormat.QR_CODE));
        String decoded=new MultiFormatReader().decode(new BinaryBitmap(new HybridBinarizer(new PlanarYUVLuminanceSource(cameraFrame,width,height,0,0,width,height,false))),hints).getText();
        if(!code.equals(decoded))throw new AssertionError();PairingConfig c=PairingConfig.parse(decoded,0);c.erase();Arrays.fill(cameraFrame,(byte)0);
        System.out.println("QrTest: offline QR encode → camera-format luminance decode → strict RS1 import passed");
    }
}
