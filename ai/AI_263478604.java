import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_263478604 extends AI {
public AI_263478604() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_a() throws LeekRunException {
ops(1);return f_k();
}
private Object f_b() throws LeekRunException {
ops(1);ops(2); return (Object) mul(f_k(), 2l);
}
private Object f_k() throws LeekRunException {
ops(1);return 5l;
}
public Object runIA(Session session) throws LeekRunException {
ops(1); return (Object) add(f_a(), f_b());
}
protected String getAIString() { return "Main_1a99d2c6fb1fa_27";}
protected String[] getErrorFiles() { return new String[] {"A", "B", "C", "Main_1a99d2c6fb1fa_27", };}

protected int[] getErrorFilesID() { return new int[] {1026, 1027, 1028, 263478604, };}

}
