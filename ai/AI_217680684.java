import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_217680684 extends AI {
public AI_217680684() throws LeekRunException {
super(2, 4);
}
public void staticInit() throws LeekRunException {
}
private double g_EPSILON = 0.0;
private boolean g_init_EPSILON = false;
private Object f_isNear(Object p_x) throws LeekRunException {var u_x = p_x;
ops(1);ops(1); return less(u_x, g_EPSILON);
}
public Object runIA(Session session) throws LeekRunException {
if (!g_init_EPSILON) { g_EPSILON = (double) 1.0E-9; g_init_EPSILON = true; ops(1); }
ops(1); return bool(f_isNear(0.5)) ? 1l : 0l;
}
protected String getAIString() { return "Main_1a99d4ced2e34_40";}
protected String[] getErrorFiles() { return new String[] {"Main_1a99d4ced2e34_40", "util", };}

protected int[] getErrorFilesID() { return new int[] {217680684, 3601347, };}

}
