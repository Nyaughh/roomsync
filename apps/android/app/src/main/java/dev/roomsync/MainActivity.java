package dev.roomsync;

import android.Manifest;
import android.app.Activity;
import android.content.*;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.os.*;
import android.text.InputType;
import android.view.*;
import android.widget.*;
import java.util.Arrays;

public final class MainActivity extends Activity {
    private final Handler handler=new Handler(Looper.getMainLooper());
    private TextView status,addresses,summary;
    private EditText host,hostPort,localPort,key,setup;
    private Spinner channel;
    private Button connect,stop,retry;
    private LinearLayout manual;
    private final Runnable refresh=new Runnable(){public void run(){
        status.setText(ReceiverService.status);connect.setEnabled(!ReceiverService.running);stop.setEnabled(ReceiverService.running);retry.setEnabled(!ReceiverService.running&&ReceiverService.pairing.available());handler.postDelayed(this,500);
    }};
    @Override public void onCreate(Bundle state){
        super.onCreate(state);getWindow().addFlags(WindowManager.LayoutParams.FLAG_SECURE);
        ScrollView scroll=new ScrollView(this);LinearLayout body=new LinearLayout(this);body.setOrientation(LinearLayout.VERTICAL);body.setPadding(dp(24),dp(28),dp(24),dp(32));body.setBackgroundColor(Color.rgb(247,246,242));scroll.addView(body);
        text(body,"ROOMSYNC  •  0.1.3",14,Color.rgb(21,125,111));text(body,"Connect this phone",29,Color.rgb(27,38,35));
        text(body,"1. Open RoomSync on Windows.\n2. Scan its QR code, or paste its private setup code.\n3. Tap Connect. Either app can be started first.",16,Color.DKGRAY);
        addresses=text(body,"This phone’s IPv4 addresses:\n"+LocalAddresses.describe(),15,Color.rgb(21,80,70));
        Button scan=button(body,"Scan Windows QR code");scan.setOnClickListener(v->startActivityForResult(new Intent(this,ScanActivity.class),21));
        setup=field(body,"Or paste setup code","RS1|192.168.1.8|5000|5001|…",InputType.TYPE_CLASS_TEXT|InputType.TYPE_TEXT_VARIATION_PASSWORD);setup.setFilters(new android.text.InputFilter[]{new android.text.InputFilter.LengthFilter(PairingConfig.MAX_CODE_LENGTH)});
        Button importCode=button(body,"Use pasted setup code");importCode.setOnClickListener(v->importCode(setup.getText().toString()));
        summary=text(body,"No pairing loaded yet.",15,Color.rgb(21,80,70));
        Button edit=button(body,"Enter or edit settings manually");
        manual=new LinearLayout(this);manual.setOrientation(LinearLayout.VERTICAL);body.addView(manual);manual.setVisibility(View.GONE);edit.setOnClickListener(v->manual.setVisibility(manual.getVisibility()==View.VISIBLE?View.GONE:View.VISIBLE));
        host=field(manual,"Windows host IPv4","192.168.1.8",InputType.TYPE_CLASS_TEXT);
        hostPort=field(manual,"Host UDP port","5000",InputType.TYPE_CLASS_NUMBER);hostPort.setText("5000");
        localPort=field(manual,"This phone’s UDP port","5001",InputType.TYPE_CLASS_NUMBER);localPort.setText("5001");
        key=field(manual,"Pairing key (64 hex characters)","Key is kept only while this app stays open",InputType.TYPE_CLASS_TEXT|InputType.TYPE_TEXT_VARIATION_PASSWORD);key.setFilters(new android.text.InputFilter[]{new android.text.InputFilter.LengthFilter(66)});
        text(body,"Output channel",14,Color.DKGRAY);channel=new Spinner(this);channel.setAdapter(new ArrayAdapter<String>(this,android.R.layout.simple_spinner_dropdown_item,new String[]{"Stereo","Left to both speakers","Right to both speakers","Mono mix"}));body.addView(channel);
        connect=button(body,"Connect & play");connect.setOnClickListener(v->connect());
        retry=button(body,"Retry saved pairing");retry.setOnClickListener(v->retry());
        stop=button(body,"Stop");stop.setOnClickListener(v->ReceiverService.requestStop(this,"Stopped by you. Tap Retry to reconnect."));
        status=text(body,ReceiverService.status,15,Color.rgb(21,80,70));
        Button copy=button(body,"Copy diagnostics (no key)");copy.setOnClickListener(v->{((android.content.ClipboardManager)getSystemService(CLIPBOARD_SERVICE)).setPrimaryClip(ClipData.newPlainText("RoomSync diagnostics",ReceiverService.diagnostics()));Toast.makeText(this,"Copied diagnostics without your pairing key",Toast.LENGTH_SHORT).show();});
        Button forget=button(body,"Forget pairing");forget.setOnClickListener(v->{ReceiverService.requestStop(this,"Stopped and forgot pairing");ReceiverService.pairing.clear();key.getText().clear();setup.getText().clear();summary.setText("Pairing forgotten. Scan or paste a new code.");if(!ReceiverService.running)ReceiverService.status="Pairing forgotten.";});
        text(body,"The QR/setup code contains your private pairing key. Keep it private. Camera images are decoded only on this phone and are never saved or uploaded.\n\nSame Wi-Fi/LAN is required. If pairing waits, compare the IPs above and check the Windows firewall’s private-network permission. Guest Wi-Fi, VPN routing or client isolation can block a LAN connection. Copy diagnostics if you need help.\n\nNo microphone, account or telemetry. Bluetooth adds unmeasured delay; use built-in or wired speakers for the first test.",13,Color.DKGRAY);
        setContentView(scroll);if(Build.VERSION.SDK_INT>=35)body.setOnApplyWindowInsetsListener((v,insets)->{android.graphics.Insets i=insets.getInsets(WindowInsets.Type.systemBars());v.setPadding(dp(24)+i.left,dp(28)+i.top,dp(24)+i.right,dp(32)+i.bottom);return insets;});
        PairingConfig saved=ReceiverService.pairing.copy();if(saved!=null){showConfig(saved,false);saved.erase();}
    }
    private void importCode(String raw){try{PairingConfig config=PairingConfig.parse(raw,channel.getSelectedItemPosition());try{ReceiverService.pairing.store(config);showConfig(config,false);setup.getText().clear();manual.setVisibility(View.GONE);ReceiverService.status="Setup loaded. Tap Connect when you’re ready.";}finally{config.erase();}}catch(IllegalArgumentException e){error(e.getMessage());}}
    private void showConfig(PairingConfig c,boolean showSecret){host.setText(c.host);hostPort.setText(Integer.toString(c.hostPort));localPort.setText(Integer.toString(c.localPort));if(showSecret)key.setText(c.key,0,c.key.length);else key.getText().clear();key.setHint("Private key loaded in memory");channel.setSelection(c.channel);summary.setText("Windows "+c.endpoint()+"\nPrivate key loaded for this app session.");}
    private void connect(){
        char[] secret=null;PairingConfig saved=null,config=null;
        try{
            if(setup.length()>0){importCode(setup.getText().toString());if(setup.length()>0)return;}
            String ip=host.getText().toString().trim();int hp=PairingConfig.parsePort(hostPort.getText().toString().trim()),lp=PairingConfig.parsePort(localPort.getText().toString().trim());
            String typed=key.getText().toString().trim();
            if(typed.isEmpty()){saved=ReceiverService.pairing.copy();if(saved==null||!saved.host.equals(ip)||saved.hostPort!=hp||saved.localPort!=lp)throw new IllegalArgumentException("Scan or paste a setup code, or enter the pairing key for these settings.");secret=saved.key.clone();}else secret=typed.toCharArray();
            config=new PairingConfig(ip,hp,lp,secret,channel.getSelectedItemPosition());start(config);config=null;key.getText().clear();
        }catch(Exception e){error(e.getMessage());}finally{if(secret!=null)Arrays.fill(secret,'\0');if(saved!=null)saved.erase();if(config!=null)config.erase();}
    }
    private void retry(){PairingConfig config=ReceiverService.pairing.copy();if(config==null){error("Scan or paste a setup code first");return;}try{showConfig(config,false);start(config);}catch(Exception e){config.erase();error(e.getMessage());}}
    private void start(PairingConfig config){ReceiverService.begin(this,config);summary.setText("Windows "+config.endpoint()+"\nPrivate key saved only in app memory for Retry.");if(Build.VERSION.SDK_INT>=33&&checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)!=PackageManager.PERMISSION_GRANTED)requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS},1);}
    private void error(String message){ReceiverService.status=message;status.setText(message);Toast.makeText(this,message,Toast.LENGTH_LONG).show();}
    @Override protected void onActivityResult(int request,int result,Intent data){super.onActivityResult(request,result,data);if(request==21&&result==RESULT_OK&&data!=null){String code=data.getStringExtra("setup");if(code!=null)importCode(code);data.removeExtra("setup");}}
    private Button button(LinearLayout b,String label){Button v=new Button(this);v.setText(label);b.addView(v);return v;}
    private EditText field(LinearLayout b,String label,String hint,int type){text(b,label,14,Color.DKGRAY);EditText e=new EditText(this);e.setSingleLine(true);e.setInputType(type);e.setHint(hint);e.setSaveEnabled(false);e.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO);b.addView(e);return e;}
    private TextView text(LinearLayout b,String value,int size,int color){TextView t=new TextView(this);t.setText(value);t.setTextSize(size);t.setTextColor(color);t.setPadding(0,dp(8),0,dp(8));b.addView(t);return t;}
    private int dp(int n){return Math.round(n*getResources().getDisplayMetrics().density);}
    @Override protected void onResume(){super.onResume();if(addresses!=null)addresses.setText("This phone’s IPv4 addresses:\n"+LocalAddresses.describe());handler.post(refresh);}
    @Override protected void onPause(){handler.removeCallbacks(refresh);super.onPause();}
}
