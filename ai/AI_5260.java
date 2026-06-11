import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_5260 extends AI {
public AI_5260() throws LeekRunException {
super(3, 3);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
Object u_i = ops(new SetLeekValue(AI_5260.this, new Object[] { 1l, 2l }), 5);
ops(Set_setPut_hx(u_i, 3l), 3);
return u_i;
}
protected String getAIString() { return "<snippet 5260>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 5260>", };}

protected int[] getErrorFilesID() { return new int[] {5260, };}

private boolean Set_setPut_hx(Object a0, Object a1) throws LeekRunException {
SetLeekValue x0; try { x0 = (SetLeekValue) (a0); } catch (ClassCastException e) { return false; }
return x0.setPut(this, a1);
}

}
