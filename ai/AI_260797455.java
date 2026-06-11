import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_260797455 extends AI {
public AI_260797455() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_a() throws LeekRunException {
ops(1);return 1l;
}
private Object f_b() throws LeekRunException {
ops(1);return 2l;
}
private Object f_c() throws LeekRunException {
ops(1);return 30l;
}
public Object runIA(Session session) throws LeekRunException {
ops(2); return (Object) add((Object) add(f_a(), f_b()), f_c());
}
protected String getAIString() { return "Main_1a99d11178335_22";}
protected String[] getErrorFiles() { return new String[] {"A", "B", "C", "Main_1a99d11178335_22", };}

protected int[] getErrorFilesID() { return new int[] {1026, 1027, 1028, 260797455, };}

}
