import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_213909368 extends AI {
public AI_213909368() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_f() throws LeekRunException {
ops(1);ops(2); return (Object) mul(f_g(), 2l);
}
private Object f_g() throws LeekRunException {
ops(1);return 5l;
}
public Object runIA(Session session) throws LeekRunException {
return f_f();
}
protected String getAIString() { return "Main_1a99cff991a6d_14";}
protected String[] getErrorFiles() { return new String[] {"A", "B", "Main_1a99cff991a6d_14", };}

protected int[] getErrorFilesID() { return new int[] {1026, 1027, 213909368, };}

}
